use anyhow::{anyhow, bail, Result};
use loka_shared::features;
use loka_shared::milestones::{Milestone, MilestoneStore, NewMilestone};
use loka_shared::world::NewEvent;
use serde_json::{json, Value};

use super::Server;

pub(super) const QUEST_TOOLS: &[&str] = &["features", "my_quest"];

const MAX_STEPS: usize = 12;
const MAX_ADDED: usize = 8;

fn pick<'a>(work: &'a [Milestone], key: &str) -> Option<&'a Milestone> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    if let Ok(n) = key.parse::<i32>() {
        return work.iter().find(|m| m.position + 1 == n);
    }
    let want = key.to_lowercase();
    work.iter().find(|m| m.title.to_lowercase() == want).or_else(|| {
        let mut close = work.iter().filter(|m| want.len() >= 3 && m.title.to_lowercase().contains(&want));
        match (close.next(), close.next()) {
            (Some(one), None) => Some(one),
            _ => None,
        }
    })
}

fn keys(args: &Value, name: &str) -> Vec<String> {
    args.get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

fn listing(list: &[Milestone]) -> String {
    let work: Vec<&Milestone> = list.iter().filter(|m| m.kind == "work").collect();
    if work.is_empty() {
        return "This quest has no milestones yet. Add 3 to 6 with add when the work has more than one real step.".into();
    }
    format!(
        "Milestones: {}.",
        work.iter().map(|m| format!("{}. {} [{}]", m.position + 1, m.title, m.status)).collect::<Vec<_>>().join("; ")
    )
}

impl Server {
    pub(super) fn quest_tools(&self, tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
        let s = |d: &str| json!({ "type": "string", "description": d });
        let mut tools = vec![tool(
            "features",
            "Look up what Loka can do and how to use it: cards and charts, media, milestones, the Run button, reminders and lists, kit, setup cards, folder sessions, parts of a quest and teams, watches and schedules, reports, quest options, failures, drafts, links, control, people, a starter team, suggested quests, Loka places. Leave feature empty for the list, or pass one name for a short how-to with the tools and an example. Read the how-to before your first use of a feature.",
            json!({ "feature": s("A feature name from the list (cards, milestones, run, board, ...) or a tool name, optional") }),
            &[],
        )];
        if self.quest.is_some() {
            tools.push(tool(
                "my_quest",
                "See and update the checklist of the quest you are working on, so your owner watches progress as it happens instead of waiting for your report. Tick a milestone the moment it is really finished (done), and add steps you discover or, when the quest has none and more than one real step, 3 to 6 of your own (add). Milestones are for watching, not approval: never stop between them. Only this quest; nothing else about it can change from here. With no arguments it lists the milestones.",
                json!({
                    "done": { "type": "array", "items": { "type": "string" }, "description": "Milestones you finished: their number or title" },
                    "add": { "type": "array", "items": { "type": "string" }, "description": "New steps, added at the end: short outcomes a person recognises" },
                    "reopen": { "type": "array", "items": { "type": "string" }, "description": "Milestones ticked too early: their number or title" }
                }),
                &[],
            ));
        }
        tools
    }

    pub(super) fn holds_tool(&self, name: &str) -> bool {
        self.listed_tools().iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name))
    }

    pub(super) fn feature_guide(&self, feature: Option<&str>) -> Result<String> {
        const OWNER: &str = "your owner";
        match feature {
            None => Ok(features::index(OWNER)),
            Some(key) => features::guide(key, OWNER, &|tool| self.holds_tool(tool)).map_err(|e| anyhow!(e)),
        }
    }

    pub(super) async fn my_quest(&self, args: &Value) -> Result<String> {
        let Some(task_id) = self.quest else {
            bail!("my_quest only works inside a quest; from your main session use update_task");
        };
        let store = MilestoneStore::new(self.store.pool().clone());
        let current = store.for_task(task_id).await?;
        let work: Vec<Milestone> = current.iter().filter(|m| m.kind == "work").cloned().collect();
        let mut notes = Vec::new();
        let mut changed = false;
        for (name, status) in [("done", "done"), ("reopen", "pending")] {
            for key in keys(args, name) {
                match pick(&work, &key) {
                    Some(found) if found.status == status => {}
                    Some(found) => {
                        store.update(found.id, None, Some(status)).await?;
                        changed = true;
                    }
                    None => notes.push(format!("no milestone matches {key}")),
                }
            }
        }
        let room = MAX_STEPS.saturating_sub(work.len());
        let wanted = keys(args, "add");
        let added: Vec<NewMilestone> = wanted
            .iter()
            .filter(|title| !work.iter().any(|m| m.title.eq_ignore_ascii_case(title)))
            .take(room.min(MAX_ADDED))
            .map(|title| NewMilestone { title: title.clone(), kind: "work".to_string() })
            .collect();
        if added.len() < wanted.len() {
            notes.push(format!("kept {} of {} new steps (a quest holds at most {MAX_STEPS}, and repeats are skipped)", added.len(), wanted.len()));
        }
        if !added.is_empty() {
            store.add(task_id, &added).await?;
            changed = true;
        }
        if changed {
            let agent = self.db.get_task(task_id).await?.and_then(|t| t.assigned_agent_id);
            let mut event = NewEvent::new("milestones.updated").task(Some(task_id)).payload(json!({ "task_id": task_id, "by": self.caller.name, "quiet": true }));
            if let Some(agent) = agent {
                event = event.agent(agent);
            }
            self.store.emit(event, None).await?;
        }
        let mut out = listing(&store.for_task(task_id).await?);
        if !notes.is_empty() {
            out.push_str(&format!(" Skipped: {}.", notes.join("; ")));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn step(title: &str, position: i32) -> Milestone {
        Milestone {
            id: Uuid::new_v4(),
            task_id: Uuid::nil(),
            position,
            title: title.into(),
            kind: "work".into(),
            status: "pending".into(),
            detail: None,
            data: json!({}),
            next_check_at: None,
            done_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn picks_steps_by_number_or_title() {
        let work = vec![step("Write the parser", 0), step("Write the tests", 1), step("Docs", 2)];
        assert_eq!(pick(&work, "2").map(|m| m.title.as_str()), Some("Write the tests"));
        assert_eq!(pick(&work, "docs").map(|m| m.position), Some(2));
        assert_eq!(pick(&work, "parser").map(|m| m.position), Some(0));
        assert!(pick(&work, "write").is_none());
        assert!(pick(&work, "9").is_none());
        assert!(pick(&work, "").is_none());
    }

    #[test]
    fn every_tool_a_feature_names_exists() {
        let sources = [
            include_str!("mcp.rs"),
            include_str!("mcp_control.rs"),
            include_str!("mcp_govern.rs"),
            include_str!("mcp_quest.rs"),
            include_str!("feeds_tools.rs"),
            include_str!("items_tools.rs"),
            include_str!("loka_tools.rs"),
            include_str!("music_tools.rs"),
            include_str!("reports_tools.rs"),
            include_str!("canvas_tools.rs"),
            include_str!("practice_tools.rs"),
            include_str!("teams_tools.rs"),
        ]
        .join("\n");
        let defined = |name: &str| sources.contains(&format!("tool(\"{name}\"")) || sources.contains(&format!("\"{name}\",\n"));
        for feature in features::FEATURES {
            for name in feature.tools {
                assert!(defined(name), "feature {} names {name}, which is not a Loka tool", feature.key);
            }
            let prose = format!("{} {}", feature.line.unwrap_or(""), feature.guide);
            for word in prose.split(|c: char| !(c.is_ascii_lowercase() || c == '_')) {
                if word.contains('_') && !word.starts_with('_') && !word.ends_with('_') && !FIELDS.contains(&word) && !loka_shared::powers::ALL.iter().any(|(power, _)| *power == word) {
                    assert!(defined(word), "feature {} mentions {word}, which is not a Loka tool", feature.key);
                }
            }
        }
    }

    const FIELDS: &[&str] = &["upload_ids", "upload_id", "remove_arrows", "start_task", "milestones_done", "part_of", "time_limit_minutes", "plan_first", "ci_feedback", "read_only", "slack_channel", "slack_thread", "attempt_variants", "review_comments", "start_line", "never_started", "cli_error", "no_report", "timed_out", "task_id", "github_feed", "live_voice", "delete_after_days", "run_logs"];
}
