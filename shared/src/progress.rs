pub fn phrase(tool: &str, input: &str) -> String {
    if let Some(text) = world_tool(tool) {
        return text.to_string();
    }
    let file = input.rsplit('/').next().unwrap_or(input).trim_matches('"');
    match tool {
        "Read" => format!("Reading {file}"),
        "Write" => format!("Writing {file}"),
        "Edit" | "MultiEdit" => format!("Editing {file}"),
        "Grep" | "Glob" => "Searching the code".to_string(),
        "Bash" => {
            let cmd = input.split_whitespace().next().unwrap_or("");
            match cmd {
                "git" => "Working with git".to_string(),
                "cargo" | "npm" | "pnpm" | "yarn" | "pytest" | "go" | "make" => format!("Running {cmd}"),
                _ => "Running a command".to_string(),
            }
        }
        "WebFetch" | "WebSearch" => "Looking something up".to_string(),
        "Agent" | "Task" => "Asking a helper".to_string(),
        "TodoWrite" => "Planning the steps".to_string(),
        t if t.contains('/') || t.starts_with("mcp__") => {
            let name = t.rsplit('/').next().and_then(|n| n.rsplit("__").next()).unwrap_or(t);
            format!("Using {name}")
        }
        t => format!("Using {t}"),
    }
}

fn world_tool(tool: &str) -> Option<&'static str> {
    let name = ["mcp__world__", "world/", "world.", "world__"]
        .iter()
        .find_map(|prefix| tool.strip_prefix(prefix))?;
    Some(match name {
        "world_agents" => "Looking around Loka",
        "agent_activity" => "Checking what someone is up to",
        "message_agent" => "Messaging another agent",
        "ask_aside" => "Asking someone on the side",
        "list_tasks" => "Checking the quest board",
        "recent_events" => "Catching up on recent events",
        "create_task" => "Setting up a quest",
        "find_expert" => "Finding the right agent",
        "my_sessions" => "Checking on its executors",
        "message_session" => "Sending a follow-up to an executor",
        "request_review" => "Asking for a review",
        "list_channels" | "read_channel" | "read_thread" => "Reading chat",
        "find_messages" => "Looking back through messages",
        "post_to_channel" => "Posting in chat",
        "create_agent" => "Creating a new agent",
        "update_agent" | "update_my_profile" => "Updating a profile",
        "update_my_notes" => "Updating notes",
        "install_skill" | "install_mcp_server" => "Installing a tool",
        "kit" | "rules" => "Checking the toolbox",
        "equip" => "Handing out a tool",
        "set_rule" => "Updating the rules",
        "grant_environment_key" => "Sharing a key",
        "create_schedule" => "Setting up a routine",
        "set_timer" => "Setting a timer",
        "suggest_actions" => "Suggesting next steps",
        "make_list" | "update_list" | "list_items" => "Working on a list",
        "place_building" | "place_decoration" | "add_critter" | "remove_item" | "place_portal" | "add_npc" | "set_ambient" => "Building in Loka",
        "create_region" | "update_region" | "list_regions" => "Shaping Loka",
        "create_challenge" | "create_course" | "create_adventure" => "Writing an Academy lesson",
        "list_challenges" | "progress_of" => "Checking Academy progress",
        _ => return None,
    })
}

pub fn from_event(kind: &str, tool: Option<&str>, text: &str) -> Option<String> {
    match (kind, tool) {
        ("tool", Some(tool)) => Some(phrase(tool, text).chars().take(120).collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{from_event, phrase};

    #[test]
    fn plain_phrases() {
        assert_eq!(phrase("Read", "…/src/world/runs.rs"), "Reading runs.rs");
        assert_eq!(phrase("mcp__world__list_tasks", "{}"), "Checking the quest board");
        assert_eq!(phrase("mcp__linear__list_issues", ""), "Using list_issues");
        assert_eq!(from_event("text", None, "hi"), None);
    }
}
