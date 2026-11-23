use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const DELIVERY_META: &str = "loka/tools";
pub const ALWAYS_LOAD_META: &str = "anthropic/alwaysLoad";
pub const FIND_TOOLS: &str = "find_tools";
pub const USE_TOOL: &str = "use_tool";

const CLAUDE_TOOL_SEARCH_SINCE: (u64, u64, u64) = (2, 1, 7);
const SEARCH_LIMIT: usize = 6;
const SUMMARY_CHARS: usize = 90;
const GATEWAY_MIN_BYTES: usize = 6 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Main,
    Quest,
    Quick,
    Aside,
    Server,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Delivery {
    #[default]
    Full,
    Native,
    Index,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Full => "full",
            Delivery::Native => "native",
            Delivery::Index => "index",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Delivery::Full),
            "native" => Some(Delivery::Native),
            "index" => Some(Delivery::Index),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        std::env::var("WORLD_TOOL_DELIVERY").ok().and_then(|value| Self::parse(&value)).unwrap_or_default()
    }

    pub fn of_message(message: &Value) -> Option<Self> {
        message.pointer("/params/_meta").and_then(|meta| meta.get(DELIVERY_META)).and_then(Value::as_str).and_then(Self::parse)
    }

    pub fn stamp(self, message: &mut Value) {
        if !matches!(message.get("method").and_then(Value::as_str), Some("tools/list" | "tools/call")) {
            return;
        }
        let Some(object) = message.as_object_mut() else { return };
        let params = object.entry("params").or_insert_with(|| json!({}));
        if let Some(params) = params.as_object_mut() {
            let meta = params.entry("_meta").or_insert_with(|| json!({}));
            if let Some(meta) = meta.as_object_mut() {
                meta.insert(DELIVERY_META.to_string(), json!(self.as_str()));
            }
        }
    }
}

pub fn use_tool_target(arguments: &Value) -> (String, Value) {
    let name = arguments.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let inner = match arguments.get("arguments") {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or_else(|_| json!({})),
        Some(value) if !value.is_null() => value.clone(),
        _ => json!({}),
    };
    (name, inner)
}

pub fn unwrap_use_tool(message: &mut Value) {
    if message.get("method").and_then(Value::as_str) != Some("tools/call") || message.pointer("/params/name").and_then(Value::as_str) != Some(USE_TOOL) {
        return;
    }
    let (name, arguments) = use_tool_target(message.pointer("/params/arguments").unwrap_or(&Value::Null));
    if name.is_empty() || name == USE_TOOL || name == FIND_TOOLS {
        return;
    }
    message["params"]["name"] = json!(name);
    message["params"]["arguments"] = arguments;
}

fn index_switched_off() -> bool {
    matches!(std::env::var("WORLD_TOOL_INDEX").ok().as_deref().map(str::trim), Some("off" | "0" | "false"))
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).filter(|part| !part.is_empty()).map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

fn claude_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    let model = model.rsplit('/').next().unwrap_or(&model);
    ["opus", "sonnet", "haiku", "fable", "claude", "best", "default"].iter().any(|prefix| model.starts_with(prefix))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeFacts {
    pub version: Option<String>,
    pub first_party: bool,
}

pub fn delivery_for(cli: &str, model: &str, claude: &ClaudeFacts) -> Delivery {
    if index_switched_off() {
        return Delivery::Full;
    }
    match cli {
        "claude" => match claude.version.as_deref().and_then(parse_version) {
            Some(version) if version >= CLAUDE_TOOL_SEARCH_SINCE && claude_model(model) && claude.first_party => Delivery::Native,
            _ => Delivery::Index,
        },
        "codex" => Delivery::Native,
        _ => Delivery::Index,
    }
}

pub fn core(role: Role) -> &'static [&'static str] {
    match role {
        Role::Main => &[
            "world_agents", "list_tasks", "create_task", "start_task", "message_agent", "my_sessions", "message_session", "find_messages", "list_reports", "get_report", "write_report", "features",
        ],
        Role::Quest => &["my_quest", "ask_agent", "message_agent", "get_report", "write_report", "show_media", "features"],
        Role::Quick => &["world_agents", "list_tasks", "create_task", "message_agent", "find_messages", "list_reports", "get_report", "write_report", "features"],
        Role::Aside => &["world_agents", "list_tasks", "agent_activity", "find_messages", "list_reports", "get_report", "features"],
        Role::Server => &[],
    }
}

fn name_of(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

fn summary(tool: &Value) -> String {
    let description = tool.get("description").and_then(Value::as_str).unwrap_or("").trim();
    let first = description.split(". ").next().unwrap_or(description).trim_end_matches('.');
    if first.chars().count() <= SUMMARY_CHARS {
        return first.to_string();
    }
    let cut: String = first.chars().take(SUMMARY_CHARS).collect();
    format!("{}...", cut.trim_end())
}

fn words(text: &str) -> Vec<String> {
    const STOP: &[&str] = &["the", "a", "an", "to", "of", "for", "and", "or", "in", "on", "my", "me", "tool", "tools"];
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() > 1 && !STOP.contains(word))
        .map(str::to_string)
        .collect()
}

fn full_entry(tool: &Value) -> String {
    format!(
        "## {}\n{}\ninputSchema: {}",
        name_of(tool),
        tool.get("description").and_then(Value::as_str).unwrap_or(""),
        tool.get("inputSchema").map(Value::to_string).unwrap_or_else(|| "{}".to_string())
    )
}

pub struct Catalog {
    role: Role,
    delivery: Delivery,
    tools: Vec<Value>,
    label: String,
}

impl Catalog {
    pub fn new(role: Role, delivery: Delivery, tools: Vec<Value>) -> Self {
        Self { role, delivery, tools, label: "Loka".to_string() }
    }

    pub fn labelled(self, label: &str) -> Self {
        Self { label: label.to_string(), ..self }
    }

    fn is_core(&self, name: &str) -> bool {
        core(self.role).contains(&name)
    }

    fn deferred(&self) -> impl Iterator<Item = &Value> {
        self.tools.iter().filter(|tool| !self.is_core(name_of(tool)))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| name_of(tool) == name)
    }

    pub fn listed(&self) -> Vec<Value> {
        match self.delivery {
            Delivery::Full => self.tools.clone(),
            Delivery::Native => self
                .tools
                .iter()
                .map(|tool| {
                    let mut tool = tool.clone();
                    if self.is_core(name_of(&tool)) {
                        tool["_meta"] = json!({ ALWAYS_LOAD_META: true });
                    }
                    tool
                })
                .collect(),
            Delivery::Index => {
                let mut listed: Vec<Value> = self.tools.iter().filter(|tool| self.is_core(name_of(tool))).cloned().collect();
                if self.deferred().next().is_some() {
                    listed.push(self.find_tools_definition());
                    listed.push(self.use_tool_definition());
                }
                listed
            }
        }
    }

    fn find_tools_definition(&self) -> Value {
        let names: Vec<&str> = self.deferred().map(name_of).collect();
        json!({
            "name": FIND_TOOLS,
            "description": format!(
                "Look up more {} tools by what you want to do and get their full definitions, then run them with use_tool. Pass words or select:name1,name2. With no query it lists every tool with a one-line summary. Other tools you can use: {}.",
                self.label,
                names.join(", ")
            ),
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string", "description": "Words to look for, or select:name1,name2 for exact tools, or empty for the full list" } }, "required": [] }
        })
    }

    fn use_tool_definition(&self) -> Value {
        json!({
            "name": USE_TOOL,
            "description": format!("Run one of the {} tools that find_tools returned. Pass its name and its arguments object exactly as its definition says. Tools already in your tool list are called directly.", self.label),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The tool name" },
                    "arguments": { "type": "object", "description": "The tool's arguments" }
                },
                "required": ["name"]
            }
        })
    }

    pub fn find(&self, query: &str) -> String {
        let query = query.trim();
        if let Some(list) = query.strip_prefix("select:") {
            return self.select(list);
        }
        if query.is_empty() {
            return self.index_listing("Every tool you can use besides the ones already in your list. Get full definitions with find_tools select:name1,name2.");
        }
        if let Some(tool) = self.tools.iter().find(|tool| name_of(tool).eq_ignore_ascii_case(query)) {
            return full_entry(tool);
        }
        let wanted = words(query);
        let mut scored: Vec<(u32, &Value)> = self
            .deferred()
            .filter_map(|tool| {
                let name = name_of(tool).to_ascii_lowercase();
                let parts: BTreeSet<&str> = name.split('_').collect();
                let description = tool.get("description").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
                let score: u32 = wanted
                    .iter()
                    .map(|word| {
                        let by_name = if name == *word {
                            100
                        } else if parts.contains(word.as_str()) {
                            20
                        } else if name.contains(word.as_str()) {
                            10
                        } else {
                            0
                        };
                        by_name + if description.contains(word.as_str()) { 2 } else { 0 }
                    })
                    .sum();
                (score > 0).then_some((score, tool))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| name_of(a.1).cmp(name_of(b.1))));
        if scored.is_empty() {
            return self.index_listing(&format!("No tool matches \"{query}\". Here is everything you can use besides the ones already in your list."));
        }
        let shown: Vec<String> = scored.iter().take(SEARCH_LIMIT).map(|(_, tool)| full_entry(tool)).collect();
        let more: Vec<&str> = scored.iter().skip(SEARCH_LIMIT).map(|(_, tool)| name_of(tool)).collect();
        let mut out = format!("{} matching tools. Run one with use_tool.\n\n{}", shown.len(), shown.join("\n\n"));
        if !more.is_empty() {
            out.push_str(&format!("\n\nAlso matching: {}. Get their definitions with find_tools select:name.", more.join(", ")));
        }
        out
    }

    fn select(&self, list: &str) -> String {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for name in list.split(',').map(str::trim).filter(|name| !name.is_empty()) {
            match self.tools.iter().find(|tool| name_of(tool) == name) {
                Some(tool) => found.push(full_entry(tool)),
                None => missing.push(name.to_string()),
            }
        }
        let mut out = found.join("\n\n");
        if !missing.is_empty() {
            let note = format!("No tool named {}. Run find_tools with words to search, or with no query to list everything.", missing.join(", "));
            out = if out.is_empty() { note } else { format!("{out}\n\n{note}") };
        }
        if out.is_empty() {
            out = self.index_listing("Name the tools after select:, for example select:create_canvas.");
        }
        out
    }

    fn index_listing(&self, lead: &str) -> String {
        let lines: Vec<String> = self.deferred().map(|tool| format!("- {}: {}", name_of(tool), summary(tool))).collect();
        format!("{lead}\n\n{}", lines.join("\n"))
    }

    pub fn unknown_note(&self, name: &str) -> String {
        let near = self.near(name);
        let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.join(", ")) };
        format!("There is no {} tool named \"{name}\".{hint} Run find_tools with words to search, or with no query to list them.", self.label)
    }

    pub fn near(&self, name: &str) -> Vec<&str> {
        let wanted = words(&name.replace('_', " "));
        let mut scored: Vec<(usize, &str)> = self
            .tools
            .iter()
            .map(name_of)
            .filter_map(|candidate| {
                let hits = wanted.iter().filter(|word| candidate.contains(word.as_str())).count();
                (hits > 0).then_some((hits, candidate))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        scored.into_iter().take(8).map(|(_, name)| name).collect()
    }
}

pub fn gateway_worthwhile(tools: &[Value]) -> bool {
    tools.iter().map(|tool| tool.to_string().len()).sum::<usize>() >= GATEWAY_MIN_BYTES
}

#[cfg(test)]
pub fn approx_tokens(value: &Value) -> usize {
    value.to_string().len().div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: &str) -> Value {
        json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": { "x": { "type": "string" } } } })
    }

    fn fixture() -> Vec<Value> {
        vec![
            tool("world_agents", "List everyone in the world. More detail."),
            tool("create_canvas", "Make a canvas report with items and arrows."),
            tool("canvas_layout", "Arrange items on a canvas."),
            tool("create_schedule", "Create a routine that wakes you on a cron."),
            tool("add_feed", "Subscribe to an RSS feed or webhook."),
        ]
    }

    fn catalog(delivery: Delivery) -> Catalog {
        Catalog::new(Role::Main, delivery, fixture())
    }

    fn listed_names(catalog: &Catalog) -> Vec<String> {
        catalog.listed().iter().map(|tool| name_of(tool).to_string()).collect()
    }

    #[test]
    fn full_lists_everything_unchanged() {
        assert_eq!(catalog(Delivery::Full).listed(), fixture());
    }

    #[test]
    fn native_marks_only_the_core_always_loaded() {
        let listed = catalog(Delivery::Native).listed();
        assert_eq!(listed.len(), 5);
        for tool in &listed {
            assert_eq!(tool.pointer("/_meta").is_some(), name_of(tool) == "world_agents", "{tool}");
        }
    }

    #[test]
    fn index_lists_core_and_the_two_meta_tools() {
        let names = listed_names(&catalog(Delivery::Index));
        assert_eq!(names, ["world_agents", FIND_TOOLS, USE_TOOL]);
    }

    #[test]
    fn index_without_deferred_tools_adds_no_meta_tools() {
        let only = Catalog::new(Role::Main, Delivery::Index, vec![tool("world_agents", "x")]);
        assert_eq!(listed_names(&only), ["world_agents"]);
    }

    #[test]
    fn the_index_names_every_deferred_tool_without_schemas() {
        let listed = catalog(Delivery::Index).listed();
        let description = listed.iter().find(|t| name_of(t) == FIND_TOOLS).unwrap()["description"].as_str().unwrap().to_string();
        for name in ["create_canvas", "canvas_layout", "create_schedule", "add_feed"] {
            assert!(description.contains(name), "{description}");
        }
        assert!(!description.contains("inputSchema"));
    }

    #[test]
    fn search_finds_rare_tools_by_words_and_returns_their_schema() {
        let catalog = catalog(Delivery::Index);
        let canvas = catalog.find("canvas");
        assert!(canvas.contains("## create_canvas") && canvas.contains("## canvas_layout") && canvas.contains("inputSchema"), "{canvas}");
        assert!(catalog.find("schedule routine").contains("## create_schedule"));
        assert!(catalog.find("rss").contains("## add_feed"));
    }

    #[test]
    fn name_matches_outrank_description_matches() {
        let tools = vec![tool("alpha", "uses the feed a lot"), tool("feed_reader", "reads")];
        let catalog = Catalog::new(Role::Aside, Delivery::Index, tools);
        let found = catalog.find("feed");
        assert!(found.find("## feed_reader").unwrap() < found.find("## alpha").unwrap(), "{found}");
    }

    #[test]
    fn a_query_that_is_a_whole_tool_name_finds_that_tool() {
        let tools = vec![tool("tool_7", "seventh"), tool("tool_70", "seventieth")];
        let out = Catalog::new(Role::Aside, Delivery::Index, tools).find("Tool_7");
        assert!(out.starts_with("## tool_7\n") && !out.contains("tool_70"), "{out}");
    }

    #[test]
    fn select_returns_exact_tools_and_reports_unknown_ones() {
        let catalog = catalog(Delivery::Index);
        let out = catalog.find("select:add_feed, nope");
        assert!(out.contains("## add_feed") && out.contains("No tool named nope"), "{out}");
        assert!(catalog.find("select:world_agents").contains("## world_agents"));
    }

    #[test]
    fn a_miss_lists_everything_instead_of_nothing() {
        let out = catalog(Delivery::Index).find("zzzz");
        assert!(out.starts_with("No tool matches \"zzzz\""), "{out}");
        assert!(out.contains("- create_canvas: Make a canvas report") && out.contains("- add_feed"), "{out}");
        assert!(!out.contains("world_agents"), "core tools are already loaded: {out}");
    }

    #[test]
    fn an_empty_query_lists_summaries() {
        let out = catalog(Delivery::Index).find("");
        assert!(out.contains("- create_schedule: Create a routine that wakes you on a cron"), "{out}");
    }

    #[test]
    fn long_matches_are_capped_and_the_rest_named() {
        let tools: Vec<Value> = (0..10).map(|i| tool(&format!("note_{i}"), "a note")).collect();
        let out = Catalog::new(Role::Aside, Delivery::Index, tools).find("note");
        assert_eq!(out.matches("\n## ").count(), SEARCH_LIMIT, "{out}");
        assert!(out.contains("Also matching: "), "{out}");
    }

    #[test]
    fn near_names_help_a_wrong_guess() {
        assert_eq!(catalog(Delivery::Index).near("make_canvas"), ["canvas_layout", "create_canvas"]);
    }

    fn facts(version: Option<&str>, first_party: bool) -> ClaudeFacts {
        ClaudeFacts { version: version.map(str::to_string), first_party }
    }

    #[test]
    fn delivery_follows_cli_model_version_and_route() {
        assert_eq!(delivery_for("claude", "opus", &facts(Some("2.1.330"), true)), Delivery::Native);
        assert_eq!(delivery_for("claude", "claude-sonnet-4-5", &facts(Some("2.1.7"), true)), Delivery::Native);
        assert_eq!(delivery_for("claude", "sonnet", &facts(Some("2.1.6"), true)), Delivery::Index);
        assert_eq!(delivery_for("claude", "sonnet", &facts(Some("2.0.76"), true)), Delivery::Index);
        assert_eq!(delivery_for("claude", "sonnet", &facts(None, true)), Delivery::Index);
        assert_eq!(delivery_for("claude", "gpt-oss", &facts(Some("2.1.330"), true)), Delivery::Index);
        assert_eq!(delivery_for("claude", "opus", &facts(Some("2.1.330"), false)), Delivery::Index);
        assert_eq!(delivery_for("codex", "gpt-5.5", &ClaudeFacts::default()), Delivery::Native);
        for cli in ["omp", "opencode", "cursor"] {
            assert_eq!(delivery_for(cli, "x", &ClaudeFacts::default()), Delivery::Index, "{cli}");
        }
    }

    #[test]
    fn delivery_round_trips_through_a_request() {
        let mut list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        Delivery::Index.stamp(&mut list);
        assert_eq!(Delivery::of_message(&list), Some(Delivery::Index));
        let mut call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "x", "arguments": {} } });
        Delivery::Native.stamp(&mut call);
        assert_eq!(call["params"]["name"], "x");
        assert_eq!(Delivery::of_message(&call), Some(Delivery::Native));
        let mut init = json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize" });
        Delivery::Index.stamp(&mut init);
        assert_eq!(Delivery::of_message(&init), None);
    }

    #[test]
    fn a_wrapped_call_unwraps_into_the_plain_call_it_names() {
        let mut call = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "use_tool", "arguments": { "name": "show_media", "arguments": { "files": ["a.png"] } } } });
        unwrap_use_tool(&mut call);
        assert_eq!(call["params"], json!({ "name": "show_media", "arguments": { "files": ["a.png"] } }));
        let mut text = json!({ "method": "tools/call", "params": { "name": "use_tool", "arguments": { "name": "x", "arguments": "{\"a\":1}" } } });
        unwrap_use_tool(&mut text);
        assert_eq!(text["params"], json!({ "name": "x", "arguments": { "a": 1 } }));
        for untouched in [json!({ "method": "tools/call", "params": { "name": "use_tool", "arguments": {} } }), json!({ "method": "tools/call", "params": { "name": "use_tool", "arguments": { "name": "use_tool" } } }), json!({ "method": "tools/list" })] {
            let mut copy = untouched.clone();
            unwrap_use_tool(&mut copy);
            assert_eq!(copy, untouched);
        }
    }

    #[test]
    fn small_servers_are_not_worth_a_gateway() {
        assert!(!gateway_worthwhile(&fixture()));
        let big: Vec<Value> = (0..30).map(|i| tool(&format!("t{i}"), &"d".repeat(300))).collect();
        assert!(gateway_worthwhile(&big));
    }
}
