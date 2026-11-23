use anyhow::{anyhow, bail, Result};
use loka_shared::canvas::{self, Canvas, Mark, Op};
use loka_shared::reports::{NewReport, Report, ReportActor};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub const CANVAS_TOOLS: &[&str] = &["create_canvas", "canvas_add_items", "canvas_update_items", "canvas_connect", "canvas_layout", "canvas_marks", "read_canvas", "search_canvases"];

fn item_schema(with_file: bool) -> Value {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "number", "description": d });
    let mut props = json!({
        "id": s("Your own short id (letters, digits, - or _) so arrows and later updates can name it; made up when left out"),
        "kind": { "type": "string", "enum": canvas::KINDS, "description": "image, video, model (GLB 3D), note (markdown card), sticky, ref (a live card for a quest, report, canvas, chat message, to-do or agent), run (bash step with a Run button), frame (a captioned group box), ink, ui (a live json-render card, the same spec as show_ui), embed (a live terminal or running quest your person watches on the board)" },
        "x": n("Left edge in canvas units; new items go in a row under everything when left out"),
        "y": n("Top edge"),
        "w": n("Width, 24 to 4000"),
        "h": n("Height, 24 to 4000"),
        "text": s("Markdown for note and sticky"),
        "caption": s("Short line under an image, video, model, frame title, or a note's label"),
        "upload_id": s("Upload id for image, video or model (show_media and chat uploads give them)"),
        "target": s("For ref: quest:<id>, report:<id>, msg:<id>, msg:ch<id>, todo:<id> or agent:<slug>"),
        "spec": { "type": "object", "description": "For ui: a json-render spec ({root, elements}) exactly as show_ui takes it; buttons and forms on it reply to you" },
        "embed": { "type": "object", "description": "For embed: {kind: terminal, target: <device id>} for a live shell on one of your person's devices (devices tool), or {kind: quest, target: <quest id>} for a running quest's live card. Browser embeds are refused for now", "properties": {
            "kind": { "type": "string", "enum": canvas::EMBED_KINDS.iter().map(|k| k.kind).collect::<Vec<_>>() },
            "target": { "type": "string" }
        }, "required": ["kind", "target"] },
        "command": s("For run: the shell command; your person gets a Run button"),
        "color": { "type": "string", "enum": canvas::COLORS, "description": "Sticky colour" },
        "locked": { "type": "boolean", "description": "Mark a settled choice as locked" }
    });
    if with_file {
        props["file"] = s("Task sessions only: path of an image, video or .glb in your work folder; it is uploaded and becomes upload_id");
    }
    json!({ "type": "object", "properties": props })
}

fn refs_schema() -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Shorthand for ref items: Loka things to place as live cards, like quest:<id>, report:<id>, msg:<id>, msg:ch<id>, todo:<id>, agent:<slug>"
    })
}

fn arrows_schema() -> Value {
    json!({
        "type": "array",
        "description": "Arrows between item ids, with an optional label",
        "items": { "type": "object", "properties": {
            "from": { "type": "string" }, "to": { "type": "string" }, "label": { "type": "string" }
        }, "required": ["from", "to"] }
    })
}

pub fn tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let canvas_ref = s("The canvas: report:<id>, its id or first characters, its title or slug");
    let items = |with_file: bool, d: &str| json!({ "type": "array", "items": item_schema(with_file), "description": d });
    vec![
        tool(
            "create_canvas",
            "Make a canvas: a board in your person's Reports library where you place images, videos, 3D models, notes, stickies, live cards of Loka things (quests, reports, chat messages, to-dos, agents, ui cards) and runnable steps, joined by arrows. Use it instead of a written report when the work is visual or a set of options to compare. Your person can pan, zoom, move things, add stickies and pin marks on items; each mark wakes you with the item it points at.",
            json!({
                "title": s("Short, specific title"),
                "items": items(true, "Items to start with"),
                "refs": refs_schema(),
                "arrows": arrows_schema(),
                "folder": s("Folder path like design/jinnfighter, optional"),
                "tags": { "type": "array", "items": { "type": "string" } },
                "public": { "type": "boolean", "description": "true lets everyone in this Loka see it" }
            }),
            &["title"],
        ),
        tool(
            "canvas_add_items",
            "Add items (and arrows between them) to a canvas. Items without x and y go in a new row under everything; follow up with canvas_layout to arrange them.",
            json!({ "canvas": canvas_ref.clone(), "items": items(true, "Items to add"), "refs": refs_schema(), "arrows": arrows_schema() }),
            &["canvas"],
        ),
        tool(
            "canvas_update_items",
            "Change, move, resize, lock or unlock, or remove canvas items and remove arrows. Each item in items names its id and only the fields to change. Rename the canvas, move its folder, share it or bring back an old revision with update_report; delete it with delete_report.",
            json!({
                "canvas": canvas_ref.clone(),
                "items": items(false, "Changes, each with the id of the item"),
                "remove": { "type": "array", "items": { "type": "string" }, "description": "Item ids to remove (their arrows go too)" },
                "remove_arrows": { "type": "array", "items": { "type": "string" }, "description": "Arrow ids to remove" }
            }),
            &["canvas"],
        ),
        tool(
            "canvas_connect",
            "Draw arrows between canvas items, optionally labelled. Drawing an arrow that exists again changes its label.",
            json!({ "canvas": canvas_ref.clone(), "arrows": arrows_schema() }),
            &["canvas", "arrows"],
        ),
        tool(
            "canvas_layout",
            "Arrange canvas items: row (left to right), grid, or flow (a row joined by arrows in the given order).",
            json!({
                "canvas": canvas_ref.clone(),
                "ids": { "type": "array", "items": { "type": "string" }, "description": "Item ids in reading order" },
                "mode": { "type": "string", "enum": canvas::LAYOUTS },
                "gap": { "type": "number", "description": "Space between items, default 48" },
                "columns": { "type": "integer", "description": "Columns for grid, default square-ish" },
                "x": { "type": "number", "description": "Where the first item goes; defaults to where the set already starts" },
                "y": { "type": "number" }
            }),
            &["canvas", "ids", "mode"],
        ),
        tool(
            "canvas_marks",
            "Read a canvas's items, arrows and your person's marks and stickies. Pass resolve with a mark id and a short reply once you have acted on it, reopen to bring one back, or remove to delete a mark that no longer applies.",
            json!({
                "canvas": canvas_ref.clone(),
                "resolve": s("Mark id to resolve, optional"),
                "reopen": s("Mark id to open again, optional"),
                "remove": s("Mark id to delete, optional"),
                "reply": s("What you changed, shown under the mark")
            }),
            &["canvas"],
        ),
        tool(
            "read_canvas",
            "Read a canvas as text: every item in reading order with quest, report, message, to-do and agent cards spelled out, the arrows and the open marks. Pass item to read one item in full with its arrows and marks.",
            json!({ "canvas": canvas_ref, "item": s("One item id to read in full, optional") }),
            &["canvas"],
        ),
        tool(
            "search_canvases",
            "Find canvases (yours and public ones) by title or by what is on them: note and sticky text, captions, commands, card targets. Lists each canvas with the items that matched.",
            json!({ "query": s("Words to look for") }),
            &["query"],
        ),
    ]
}

fn list_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default()
}

fn items_arg(args: &Value, key: &str) -> Result<Vec<canvas::ItemInput>> {
    let Some(list) = args.get(key) else { return Ok(Vec::new()) };
    let mut list = list.clone();
    if let Some(items) = list.as_array_mut() {
        for item in items.iter_mut() {
            if item.get("file").is_some() {
                bail!("file works only in task sessions on a device; put the image in chat or use show_media and pass its upload_id");
            }
            for field in ["text", "caption"] {
                if let Some(text) = item.get(field).and_then(Value::as_str).map(super::prompts::plain_dashes) {
                    item[field] = Value::String(text);
                }
            }
        }
    }
    serde_json::from_value(list).map_err(|e| anyhow!("could not read {key}: {e}"))
}

fn refs_arg(args: &Value) -> Result<Vec<canvas::ItemInput>> {
    list_arg(args, "refs")
        .into_iter()
        .map(|r| {
            let target = r.trim().to_string();
            if !canvas::ref_ok(&target) {
                bail!("{target} is not a Loka ref; use quest:<id>, report:<id>, msg:<id>, msg:ch<id>, todo:<id> or agent:<slug>");
            }
            Ok(canvas::ItemInput { kind: Some("ref".into()), target: Some(target), ..Default::default() })
        })
        .collect()
}

fn with_refs(args: &Value) -> Result<Vec<canvas::ItemInput>> {
    let mut items = items_arg(args, "items")?;
    items.extend(refs_arg(args)?);
    Ok(items)
}

async fn ref_notes(store: &WorldStore, owner: Uuid, org: Uuid, canvas: &Canvas, ids: &[String]) -> Result<String> {
    let targets: Vec<String> = canvas.items.iter().filter(|i| ids.contains(&i.id)).filter_map(|i| i.target.clone().or_else(|| i.embed.as_ref().filter(|e| e.kind == "quest").map(|e| format!("quest:{}", e.target)))).collect();
    if targets.is_empty() {
        return Ok(String::new());
    }
    let found = store.describe_refs(owner, org, &targets).await?;
    let lines: Vec<String> = targets
        .iter()
        .map(|t| match found.get(t) {
            Some(line) => format!("- {t}: {line}"),
            None => format!("- {t}: not found or not visible to your person; the card will say so"),
        })
        .collect();
    Ok(format!("\nCards:\n{}", lines.join("\n")))
}

fn arrows_arg(args: &Value) -> Result<Vec<canvas::EdgeInput>> {
    match args.get("arrows") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| anyhow!("could not read arrows: {e}")),
    }
}

async fn find(store: &WorldStore, owner: Uuid, org: Uuid, key: Option<&str>) -> Result<Report> {
    let key = key.ok_or_else(|| anyhow!("canvas is required"))?;
    let report = store.find_report(owner, org, key).await?.ok_or_else(|| anyhow!("no canvas matches {key}; see list_reports"))?;
    if report.kind != "canvas" {
        bail!("\"{}\" is a written report; make a canvas with create_canvas", report.title);
    }
    if report.owner_user_id != owner {
        bail!("\"{}\" belongs to someone else; you can read it but not change it", report.title);
    }
    Ok(report)
}

fn mark_line(m: &Mark, canvas: &Canvas) -> String {
    let near = (m.kind == "sticky").then(|| canvas.near_item(m.item_id.as_deref(), (m.x, m.y))).flatten();
    let at = match (near.as_deref().and_then(|i| canvas.item_line(i)), canvas.sticky_of(m.item_id.as_deref())) {
        (Some(line), Some(sticky)) => format!(" on {line} (sticky {sticky})"),
        (Some(line), None) => format!(" on {line}"),
        _ => m.item_id.as_deref().and_then(|i| canvas.item_line(i)).map(|l| format!(" on {l}")).unwrap_or_default(),
    };
    let reply = m.reply.as_deref().map(|r| format!(" (reply: {r})")).unwrap_or_default();
    format!("- {} {} {}{at}: {}{reply}", m.status, m.kind, m.id, m.body.split_whitespace().collect::<Vec<_>>().join(" "))
}

pub async fn view(store: &WorldStore, owner: Uuid, org: Uuid, report: &Report) -> Result<String> {
    let canvas = Canvas::parse(report.canvas.as_ref())?;
    let marks = store.canvas_marks(report.id).await?;
    let found = store.describe_refs(owner, org, &canvas.targets()).await?;
    let open = marks.iter().filter(|m| m.status == "open").count();
    let mut out = format!(
        "Canvas report:{} \"{}\" revision {}: {} items, {} arrows, {open} open marks.\n{}",
        report.id,
        report.title,
        report.revision,
        canvas.items.len(),
        canvas.edges.len(),
        canvas.outline()
    );
    if !found.is_empty() {
        let mut cards: Vec<String> = found.iter().map(|(t, line)| format!("- {t}: {line}")).collect();
        cards.sort();
        out.push_str("\n\nCards on it:\n");
        out.push_str(&cards.join("\n"));
    }
    if !marks.is_empty() {
        out.push_str("\n\nMarks from your person:\n");
        out.push_str(&marks.iter().rev().take(40).map(|m| mark_line(m, &canvas)).collect::<Vec<_>>().join("\n"));
    }
    Ok(out)
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, quest: Option<Uuid>, name: &str, args: &Value) -> Result<String> {
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("you have no person to keep canvases for"))?;
    let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str);
    let by = ReportActor::Agent(caller.id);
    let change = |ops: Vec<Op>| async move {
        let report = find(store, owner, org, arg("canvas")).await?;
        store.change_canvas(report.id, by, ops).await?.ok_or_else(|| anyhow!("that canvas is gone"))
    };
    match name {
        "create_canvas" => {
            let mut canvas = Canvas::default();
            canvas.apply_ops(vec![Op::Add { items: with_refs(args)?, edges: arrows_arg(args)? }], "agent")?;
            let report = store
                .create_canvas(
                    NewReport {
                        owner_user_id: owner,
                        by,
                        title: arg("title").unwrap_or("").to_string(),
                        body: String::new(),
                        folder: arg("folder").map(String::from),
                        tags: list_arg(args, "tags"),
                        visibility: args.get("public").and_then(Value::as_bool).filter(|p| *p).map(|_| "public".to_string()),
                        pinned: false,
                        task_id: quest,
                    },
                    canvas,
                )
                .await?;
            let made = Canvas::parse(report.canvas.as_ref())?;
            let ids: Vec<String> = made.items.iter().map(|i| i.id.clone()).collect();
            Ok(format!(
                "Made canvas report:{} \"{}\". Items:\n{}{}\nTell your person in a line or two and put report:{} on its own line so it shows as a card.",
                report.id,
                report.title,
                made.outline(),
                ref_notes(store, owner, org, &made, &ids).await?,
                report.id
            ))
        }
        "canvas_add_items" => {
            let items = with_refs(args)?;
            if items.is_empty() {
                bail!("give items or refs to add");
            }
            let (report, added) = change(vec![Op::Add { items, edges: arrows_arg(args)? }]).await?;
            let notes = ref_notes(store, owner, org, &Canvas::parse(report.canvas.as_ref())?, &added).await?;
            Ok(format!("Added {} to report:{}, now revision {}: {}{notes}", added.len(), report.id, report.revision, added.join(", ")))
        }
        "canvas_update_items" => {
            let mut ops = Vec::new();
            let items = items_arg(args, "items")?;
            if !items.is_empty() {
                ops.push(Op::Update { items });
            }
            let remove = list_arg(args, "remove");
            if !remove.is_empty() {
                ops.push(Op::Remove { ids: remove });
            }
            let arrows = list_arg(args, "remove_arrows");
            if !arrows.is_empty() {
                ops.push(Op::Disconnect { ids: arrows });
            }
            if ops.is_empty() {
                bail!("give items to change, remove or remove_arrows");
            }
            let (report, _) = change(ops).await?;
            Ok(format!("Updated report:{}, now revision {}.", report.id, report.revision))
        }
        "canvas_connect" => {
            let (report, _) = change(vec![Op::Connect { edges: arrows_arg(args)? }]).await?;
            Ok(format!("Connected on report:{}, now {} arrows.", report.id, Canvas::parse(report.canvas.as_ref())?.edges.len()))
        }
        "canvas_layout" => {
            let ops = vec![Op::Layout {
                ids: list_arg(args, "ids"),
                mode: arg("mode").unwrap_or("row").to_string(),
                gap: args.get("gap").and_then(Value::as_f64),
                columns: args.get("columns").and_then(Value::as_u64).map(|c| c as usize),
                x: args.get("x").and_then(Value::as_f64),
                y: args.get("y").and_then(Value::as_f64),
            }];
            let (report, _) = change(ops).await?;
            Ok(format!("Arranged on report:{}, now revision {}.", report.id, report.revision))
        }
        "canvas_marks" => {
            let report = find(store, owner, org, arg("canvas")).await?;
            for (key, reopen) in [("resolve", false), ("reopen", true)] {
                if let Some(raw) = arg(key) {
                    let id = Uuid::parse_str(raw.trim()).map_err(|_| anyhow!("{key} takes a mark id from canvas_marks"))?;
                    store
                        .resolve_canvas_mark(&report, id, by, arg("reply").map(super::prompts::plain_dashes), reopen)
                        .await?
                        .ok_or_else(|| anyhow!("no mark {id} on this canvas"))?;
                }
            }
            if let Some(raw) = arg("remove") {
                let id = Uuid::parse_str(raw.trim()).map_err(|_| anyhow!("remove takes a mark id from canvas_marks"))?;
                if !store.remove_canvas_mark(&report, id, by).await? {
                    bail!("no mark {id} on this canvas");
                }
            }
            view(store, owner, org, &report).await
        }
        "read_canvas" => {
            let key = arg("canvas").ok_or_else(|| anyhow!("canvas is required"))?;
            let report = store.find_report(owner, org, key).await?.ok_or_else(|| anyhow!("no canvas matches {key}; see search_canvases"))?;
            if report.kind != "canvas" {
                bail!("\"{}\" is a written report; read it with get_report", report.title);
            }
            let Some(item) = arg("item").map(str::trim).filter(|i| !i.is_empty()) else {
                let (_, _, digest) = store.canvas_digest(owner, org, &report, 24_000).await?;
                let marks = store.canvas_marks(report.id).await?;
                let open: Vec<String> = marks.iter().filter(|m| m.status == "open").map(|m| mark_line(m, &Canvas::parse(report.canvas.as_ref()).unwrap_or_default())).collect();
                let tail = if open.is_empty() { String::new() } else { format!("\n\nOpen marks from your person:\n{}", open.join("\n")) };
                return Ok(format!("{digest}{tail}\n\nIds and positions: canvas_marks or get_report."));
            };
            let (canvas, found, _) = store.canvas_digest(owner, org, &report, 0).await?;
            let line = canvas.describe_item(item, &found, 8000).ok_or_else(|| anyhow!("no item {item} on this canvas"))?;
            let arrows: Vec<String> = canvas
                .edges
                .iter()
                .filter(|e| e.from == item || e.to == item)
                .map(|e| format!("- arrow {} {} -> {}{}", e.id, e.from, e.to, e.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default()))
                .collect();
            let marks: Vec<String> = store.canvas_marks(report.id).await?.iter().filter(|m| m.item_id.as_deref() == Some(item)).map(|m| mark_line(m, &canvas)).collect();
            let mut out = format!("On canvas report:{} \"{}\": {line}", report.id, report.title);
            if !arrows.is_empty() {
                out.push_str(&format!("\nArrows:\n{}", arrows.join("\n")));
            }
            if !marks.is_empty() {
                out.push_str(&format!("\nMarks:\n{}", marks.join("\n")));
            }
            Ok(out)
        }
        "search_canvases" => {
            let query = arg("query").map(str::trim).filter(|q| q.chars().count() >= 2).ok_or_else(|| anyhow!("query needs at least two characters"))?;
            let hits = store.search_canvas_items(owner, org, query, 20).await?;
            if hits.is_empty() {
                return Ok(format!("No canvas mentions \"{query}\"."));
            }
            Ok(hits
                .iter()
                .map(|h| {
                    let items: Vec<String> = h
                        .items
                        .as_array()
                        .map(|list| list.iter().map(|i| format!("    - {} {}: {}", i["kind"].as_str().unwrap_or(""), i["id"].as_str().unwrap_or(""), i["label"].as_str().unwrap_or(""))).collect())
                        .unwrap_or_default();
                    let folder = if h.folder.is_empty() { String::new() } else { format!(" in {}", h.folder) };
                    format!("- report:{} \"{}\"{folder}{}{}", h.id, h.title, if items.is_empty() { "" } else { "\n" }, items.join("\n"))
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        _ => bail!("unknown canvas tool {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn refs_become_ref_items_and_bad_refs_are_refused() {
        let items = with_refs(&json!({ "items": [{ "kind": "note", "text": "Plan" }], "refs": ["quest:abcdef12", " agent:tara ", "msg:ch42"] })).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].kind.as_deref(), Some("note"));
        let refs: Vec<(Option<&str>, Option<&str>)> = items[1..].iter().map(|i| (i.kind.as_deref(), i.target.as_deref())).collect();
        assert_eq!(refs, vec![(Some("ref"), Some("quest:abcdef12")), (Some("ref"), Some("agent:tara")), (Some("ref"), Some("msg:ch42"))]);
        assert!(refs_arg(&json!({})).unwrap().is_empty());
        for bad in ["https://example.com", "quest:", "nonsense:abcdef12", "quest:abc def"] {
            let err = refs_arg(&json!({ "refs": [bad] })).unwrap_err().to_string();
            assert!(err.contains("is not a Loka ref"), "{bad}: {err}");
        }
    }

    #[test]
    fn embed_items_read_from_tool_input_and_the_schema_lists_every_kind() {
        let items = items_arg(&json!({ "items": [{ "kind": "embed", "embed": { "kind": "quest", "target": "abcdef12" }, "caption": "Watch it" }] }), "items").unwrap();
        let embed = items[0].embed.as_ref().unwrap();
        assert_eq!((items[0].kind.as_deref(), embed.kind.as_str(), embed.target.as_str()), (Some("embed"), "quest", "abcdef12"));
        assert!(items_arg(&json!({ "items": [{ "kind": "embed", "embed": { "kind": "quest" } }] }), "items").is_err());
        let schema = item_schema(false);
        assert_eq!(schema["properties"]["embed"]["properties"]["kind"]["enum"], json!(["terminal", "quest", "browser"]));
        assert!(schema["properties"]["kind"]["enum"].as_array().unwrap().contains(&json!("embed")));
        let mut canvas = Canvas::default();
        let refused = canvas.apply_ops(vec![Op::Add { items: items_arg(&json!({ "items": [{ "kind": "embed", "embed": { "kind": "browser", "target": "https://example.com" } }] }), "items").unwrap(), edges: vec![] }], "agent");
        assert!(refused.unwrap_err().to_string().contains("cannot be watched inside Loka yet"));
    }

    #[tokio::test]
    async fn tools_read_search_add_refs_and_remove_marks() -> Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Tool owner') RETURNING id")
            .bind(format!("{tag}@tools.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Mason', 'claude', 'opus', $2) RETURNING id")
            .bind(format!("mason-{}", &tag[..8]))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let caller = store.get_agent(agent).await?.expect("agent");
        let org = caller.org_id.expect("org");
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, repo, org_id, assigned_agent_id) VALUES ($1, 'Raise the tower', 'acme/site', $2, $3) RETURNING id")
            .bind(owner)
            .bind(org)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let todo: Uuid = sqlx::query_scalar("INSERT INTO world_todos (owner_user_id, text, org_id) VALUES ($1, 'Order the stones', $2) RETURNING id")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let word = format!("moat{}", &tag[..6]);

        let made = call(&store, &caller, None, "create_canvas", &json!({
            "title": "Siege plan",
            "items": [{ "id": "n", "kind": "note", "text": format!("Fill the {word} first") }],
            "refs": [format!("quest:{quest}"), format!("todo:{todo}")]
        }))
        .await?;
        assert!(made.contains("Raise the tower") && made.contains("Order the stones"), "{made}");
        let report: Uuid = sqlx::query_scalar("SELECT id FROM world_reports WHERE owner_user_id = $1 AND title = 'Siege plan'").bind(owner).fetch_one(&pool).await?;
        let canvas = report.to_string();

        let refused = call(&store, &caller, None, "canvas_add_items", &json!({ "canvas": canvas, "refs": ["nonsense:abcdef12"] })).await.unwrap_err();
        assert!(refused.to_string().contains("is not a Loka ref"));
        let empty = call(&store, &caller, None, "canvas_add_items", &json!({ "canvas": canvas })).await.unwrap_err();
        assert!(empty.to_string().contains("give items or refs"));
        let added = call(&store, &caller, None, "canvas_add_items", &json!({ "canvas": canvas, "refs": [format!("agent:{}", caller.slug)] })).await?;
        assert!(added.contains("Agent Mason"), "{added}");

        let whole = call(&store, &caller, None, "read_canvas", &json!({ "canvas": canvas })).await?;
        assert!(whole.contains(&word) && whole.contains("Raise the tower") && whole.contains("Order the stones") && whole.contains("Agent Mason"), "{whole}");
        let one = call(&store, &caller, None, "read_canvas", &json!({ "canvas": canvas, "item": "n" })).await?;
        assert!(one.starts_with(&format!("On canvas report:{report} \"Siege plan\": note n")), "{one}");
        assert!(call(&store, &caller, None, "read_canvas", &json!({ "canvas": canvas, "item": "nope" })).await.is_err());

        let found = call(&store, &caller, None, "search_canvases", &json!({ "query": word })).await?;
        assert!(found.contains(&format!("report:{report}")) && found.contains("note n"), "{found}");
        let none = call(&store, &caller, None, "search_canvases", &json!({ "query": format!("{word}zz") })).await?;
        assert!(none.starts_with("No canvas mentions"));

        let full = store.get_report(report).await?.expect("report");
        let mark = store.add_canvas_mark(&full, owner, canvas::NewMark { item_id: Some("n".into()), body: "dig deeper".into(), ..Default::default() }).await?;
        let listed = call(&store, &caller, None, "canvas_marks", &json!({ "canvas": canvas })).await?;
        assert!(listed.contains(&mark.id.to_string()) && listed.contains("dig deeper"), "{listed}");
        let after = call(&store, &caller, None, "canvas_marks", &json!({ "canvas": canvas, "remove": mark.id.to_string() })).await?;
        assert!(!after.contains("dig deeper"), "{after}");
        assert!(store.canvas_marks(report).await?.is_empty());
        let again = call(&store, &caller, None, "canvas_marks", &json!({ "canvas": canvas, "remove": mark.id.to_string() })).await.unwrap_err();
        assert!(again.to_string().contains("no mark"));

        sqlx::query("DELETE FROM world_events WHERE agent_id = $1 OR payload->>'owner_user_id' = $2").bind(agent).bind(owner.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_inbox WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM world_todos WHERE id = $1").bind(todo).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(quest).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }
}
