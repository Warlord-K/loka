use anyhow::{anyhow, bail, Result};
use loka_shared::reports::{NewReport, Report, ReportActor, ReportBrief, ReportFilter, ReportPatch};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub const REPORT_TOOLS: &[&str] = &["write_report", "update_report", "get_report", "list_reports", "delete_report"];
pub const READ_TOOLS: &[&str] = &["get_report", "list_reports"];
pub const QUEST_WRITE_TOOLS: &[&str] = &["write_report", "update_report"];
const READ_CHARS: usize = 60_000;

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let tags = json!({ "type": "array", "items": { "type": "string" }, "description": "Up to 12 short tags like perf or q3-planning" });
    vec![
        tool(
            "write_report",
            "Write a report: a lasting markdown document your person reads in the Reports library (Hilltop Library in Loka). Use it for anything long, like findings, analysis, research, comparisons, plans, postmortems or meeting notes, instead of pasting long text into chat. Use headings, lists, tables and code blocks. Link other reports with [[Their title]]; backlinks appear automatically. Afterwards post a two or three line summary in chat with report:<id> on its own line, which shows as a card. Reports are private to your person unless public is true. For dashboards, comparisons or choices, embed interactive cards: a fenced code block with language ui holding a JSON spec in the show_ui format (root, elements, state; same components and actions). Its reply and submit buttons send the reader's answer to you as a chat message naming the report, so a report can end with the decisions it argues for (start a quest, connect a tool, pick an option) and the person approves them in place. A fenced bash block gets a Run button for your owner (its output comes to your chat), and setup:<key> alone on a line shows a one-click setup card.",
            json!({
                "title": s("Short, specific title; it is also how [[links]] find it"),
                "body": s("The full markdown body, up to 512 KB"),
                "folder": s("Folder path like research/infra, optional"),
                "tags": tags.clone(),
                "pinned": { "type": "boolean", "description": "Pin it to the top of the library" },
                "public": { "type": "boolean", "description": "true lets everyone in this Loka read it" }
            }),
            &["title", "body"],
        ),
        tool(
            "update_report",
            "Change a report: rewrite the body, append a section, retitle it, move it to another folder, retag, pin or change who can read it. Prefer append for adding findings to a running report. Every change keeps a revision your person can restore.",
            json!({
                "report": s("report:<id>, its id or first characters, its exact title or slug"),
                "body": s("Replace the whole body"),
                "append": s("Markdown added at the end"),
                "title": s("New title"),
                "folder": s("New folder path, empty string for the top level"),
                "tags": tags,
                "pinned": { "type": "boolean" },
                "visibility": s("private or public"),
                "restore_revision": { "type": "integer", "description": "Bring back this older revision's title, body, folder and tags (get_report with history lists them); the current text stays as a revision too. Not with body, append, title, folder or tags" }
            }),
            &["report"],
        ),
        tool(
            "get_report",
            "Read a report with its links and backlinks. Long reports come in pages; pass offset to continue. revision reads an older version; history lists the kept revisions instead.",
            json!({
                "report": s("report:<id>, its id or first characters, its exact title or slug"),
                "offset": { "type": "integer", "description": "Character offset to start reading from" },
                "revision": { "type": "integer", "description": "Read this older revision instead" },
                "history": { "type": "boolean", "description": "List the kept revisions: number, when, who, title and size" }
            }),
            &["report"],
        ),
        tool(
            "list_reports",
            "List or search your person's reports and workspaces (and public ones): title, id, folder, tags and when it changed. query searches titles, bodies and tags. Written pages live in Reports; workspace boards live in Workspaces.",
            json!({
                "query": s("Words to look for, optional"),
                "folder": s("Only this folder and its subfolders, optional"),
                "tag": s("Only reports with this tag, optional"),
                "kind": { "type": "string", "enum": ["doc", "canvas"], "description": "doc lists written reports; canvas lists workspaces" },
                "practice": { "type": "boolean", "description": "true lists only practice workspaces, false leaves them out" },
                "limit": { "type": "integer", "description": "At most this many, default 30" }
            }),
            &[],
        ),
        tool(
            "delete_report",
            "Delete a report for good, with its history. Only when your person asks or it is clearly a mistake or duplicate; links to it in other reports become plain text.",
            json!({ "report": s("report:<id>, its id or first characters, its exact title or slug") }),
            &["report"],
        ),
    ]
}

fn line(r: &ReportBrief) -> String {
    let mut extra = vec![format!("updated {}", r.updated_at.format("%Y-%m-%d %H:%M UTC"))];
    if !r.folder.is_empty() {
        extra.push(format!("in {}", r.folder));
    }
    if !r.tags.is_empty() {
        extra.push(format!("#{}", r.tags.join(" #")));
    }
    if r.pinned {
        extra.push("pinned".into());
    }
    if let Some(subject) = r.practice.as_ref().and_then(|p| p.get("subject")).and_then(Value::as_str) {
        extra.push(format!("{subject} practice workspace lesson, list_challenges"));
    } else if r.kind == "canvas" {
        extra.push(if r.open_marks > 0 { format!("workspace, {} open marks", r.open_marks) } else { "workspace".into() });
    }
    if r.visibility == "public" {
        extra.push("public".into());
    }
    format!("- report:{} \"{}\" ({})", r.id, r.title, extra.join(", "))
}

fn page(body: &str, offset: usize) -> (String, Option<usize>) {
    let start = body.char_indices().nth(offset).map(|(i, _)| i).unwrap_or(body.len());
    let rest = &body[start..];
    match rest.char_indices().nth(READ_CHARS) {
        Some((end, _)) => (rest[..end].to_string(), Some(offset + READ_CHARS)),
        None => (rest.to_string(), None),
    }
}

async fn find(store: &WorldStore, owner: uuid::Uuid, org: uuid::Uuid, key: Option<&str>) -> Result<Report> {
    let key = key.ok_or_else(|| anyhow!("report is required"))?;
    store.find_report(owner, org, key).await?.ok_or_else(|| anyhow!("no report matches {key}; see list_reports"))
}

fn tags_arg(args: &Value) -> Option<Vec<String>> {
    args.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, quest: Option<Uuid>, name: &str, args: &Value) -> Result<String> {
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("you have no person to keep reports for"))?;
    let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str);
    let by = ReportActor::Agent(caller.id);
    let editable = |r: &Report| -> Result<()> {
        if r.owner_user_id != owner {
            bail!("\"{}\" belongs to someone else; you can read it but not change it", r.title);
        }
        Ok(())
    };
    match name {
        "write_report" => {
            let body = super::prompts::plain_dashes(arg("body").ok_or_else(|| anyhow!("body is required"))?);
            loka_shared::ui_cards::check_markdown(&body)?;
            let report = store
                .create_report(NewReport {
                    owner_user_id: owner,
                    by,
                    title: arg("title").unwrap_or("").to_string(),
                    body,
                    folder: arg("folder").map(String::from),
                    tags: tags_arg(args).unwrap_or_default(),
                    visibility: args.get("public").and_then(Value::as_bool).filter(|p| *p).map(|_| "public".to_string()),
                    pinned: args.get("pinned").and_then(Value::as_bool).unwrap_or(false),
                    task_id: quest,
                })
                .await?;
            Ok(format!(
                "Saved report:{} \"{}\"{} ({} links). Tell your person in two or three lines what it says and put report:{} on its own line so it shows as a card.",
                report.id,
                report.title,
                if report.folder.is_empty() { String::new() } else { format!(" in {}", report.folder) },
                report.links.len(),
                report.id
            ))
        }
        "update_report" => {
            let report = find(store, owner, org, arg("report")).await?;
            editable(&report)?;
            if let Some(n) = args.get("restore_revision").and_then(Value::as_i64) {
                if ["body", "append", "title", "folder", "tags"].iter().any(|k| args.get(*k).is_some_and(|v| !v.is_null())) {
                    bail!("restore_revision replaces title, body, folder and tags, so give it alone (pinned and visibility can follow in another call)");
                }
                let restored = store
                    .restore_report_revision(report.id, n as i32, by)
                    .await?
                    .ok_or_else(|| anyhow!("revision {n} is not kept; get_report with history lists the kept ones"))?;
                return Ok(format!("Restored report:{} \"{}\" to revision {n}, now revision {}.", restored.id, restored.title, restored.revision));
            }
            if report.kind == "canvas" && (arg("body").is_some() || arg("append").is_some()) {
                bail!("\"{}\" is a workspace; change it with canvas_add_items, canvas_update_items, canvas_connect and canvas_layout", report.title);
            }
            for text in [arg("body"), arg("append")].into_iter().flatten() {
                loka_shared::ui_cards::check_markdown(text)?;
            }
            let patch = ReportPatch {
                title: arg("title").map(String::from),
                body: arg("body").map(super::prompts::plain_dashes),
                append: arg("append").map(super::prompts::plain_dashes),
                folder: arg("folder").map(String::from),
                tags: tags_arg(args),
                pinned: args.get("pinned").and_then(Value::as_bool),
                visibility: arg("visibility").map(String::from),
                base_updated_at: None,
                canvas: None,
            };
            let updated = store.update_report(report.id, by, &patch).await?.ok_or_else(|| anyhow!("that report is gone"))?;
            Ok(format!("Updated report:{} \"{}\", now revision {}.", updated.id, updated.title, updated.revision))
        }
        "get_report" => {
            let report = find(store, owner, org, arg("report")).await?;
            if args.get("history").and_then(Value::as_bool).unwrap_or(false) {
                let revisions = store.report_revisions(report.id).await?;
                let mut out = vec![format!("report:{} \"{}\" is at revision {}. Kept revisions, newest first:", report.id, report.title, report.revision)];
                for r in revisions {
                    let who = match (r.author_agent_id, r.author_user_id) {
                        (Some(a), _) if a == caller.id => "you".to_string(),
                        (Some(a), _) => store.get_agent(a).await?.map(|a| a.name).unwrap_or_else(|| "an agent".into()),
                        (None, Some(_)) => "a person".to_string(),
                        (None, None) => "unknown".to_string(),
                    };
                    out.push(format!("- revision {} {} by {who}: \"{}\" ({} bytes)", r.revision, r.created_at.format("%Y-%m-%d %H:%M UTC"), r.title, r.size));
                }
                return Ok(out.join("\n"));
            }
            if report.kind == "canvas" && args.get("revision").is_none() {
                return super::canvas_tools::view(store, owner, org, &report).await;
            }
            let (title, body, revision) = match args.get("revision").and_then(Value::as_i64) {
                Some(n) => {
                    let old = store
                        .report_revision(report.id, n as i32)
                        .await?
                        .ok_or_else(|| anyhow!("revision {n} is not kept; this report is at revision {}", report.revision))?;
                    (old.title, old.body, old.revision)
                }
                None => (report.title.clone(), report.body.clone(), report.revision),
            };
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let (text, next) = page(&body, offset);
            let mut out = format!("# {title}\nreport:{} revision {revision}, updated {}", report.id, report.updated_at.format("%Y-%m-%d %H:%M UTC"));
            if !report.folder.is_empty() {
                out.push_str(&format!(", in {}", report.folder));
            }
            if !report.tags.is_empty() {
                out.push_str(&format!(", #{}", report.tags.join(" #")));
            }
            out.push_str("\n\n");
            out.push_str(&text);
            if let Some(n) = next {
                out.push_str(&format!("\n\n[continues; call get_report with offset {n}]"));
            }
            let links = store.report_outlinks(owner, org, &report).await?;
            if !links.is_empty() {
                out.push_str(&format!("\n\nLinks to: {}", links.iter().map(|l| format!("\"{}\"", l.title)).collect::<Vec<_>>().join(", ")));
            }
            let back = store.report_backlinks(owner, org, report.id).await?;
            if !back.is_empty() {
                out.push_str(&format!("\nLinked from: {}", back.iter().map(|l| format!("\"{}\"", l.title)).collect::<Vec<_>>().join(", ")));
            }
            Ok(out)
        }
        "list_reports" => {
            let filter = ReportFilter {
                query: arg("query").map(String::from),
                folder: arg("folder").map(String::from),
                tag: arg("tag").map(String::from),
                limit: Some(args.get("limit").and_then(Value::as_i64).unwrap_or(30).clamp(1, 100)),
                offset: None,
                practice: args.get("practice").and_then(Value::as_bool),
                kind: arg("kind").map(String::from),
            };
            let found = store.list_reports(owner, org, &filter).await?;
            Ok(if found.is_empty() { "No reports matched.".into() } else { found.iter().map(line).collect::<Vec<_>>().join("\n") })
        }
        "delete_report" => {
            let report = find(store, owner, org, arg("report")).await?;
            editable(&report)?;
            store.delete_report(report.id, by).await?;
            Ok(format!("Deleted \"{}\".", report.title))
        }
        _ => bail!("unknown report tool {name}"),
    }
}
