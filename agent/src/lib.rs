extern crate self as loka_agent;

#[cfg(test)]
mod test_env;
#[cfg(test)]
fn passed_on_to_clis(_: &str) -> bool {
    false
}

#[path = "world/prompts.rs"]
mod prompts;

#[path = "world/feeds_tools.rs"]
mod feeds_tools;

#[path = "world/items_tools.rs"]
mod items_tools;

#[path = "world/loka_tools.rs"]
mod loka_tools;

#[path = "world/music_tools.rs"]
mod music_tools;

#[path = "world/reports_tools.rs"]
mod reports_tools;

#[path = "world/canvas_tools.rs"]
mod canvas_tools;

#[path = "world/practice_tools.rs"]
mod practice_tools;

#[path = "world/teams_tools.rs"]
mod teams_tools;

#[path = "world/tool_catalog.rs"]
pub mod tool_catalog;

#[path = "world/mcp.rs"]
pub mod mcp;

#[path = "world/quests.rs"]
mod quests;

#[path = "world/runner_server.rs"]
pub mod runner_server;
