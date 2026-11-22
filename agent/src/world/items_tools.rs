use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Duration, Utc};
use loka_shared::items::{NewItem, NewList};
use loka_shared::todos::{age_label, clean_proposals, priority_from, NewTodo, Todo, TodoActor, PRIORITIES};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

const DECORATIONS: &[&str] = &[
    "tree", "blossom", "autumn", "pine", "bush", "flowers", "rock", "lamp", "bench", "planter", "crate", "barrel", "sign",
];

const REMINDERS_PER_DAY: i64 = 48;

fn hex(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|c| c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit()))
        .map(String::from)
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn match_index(items: &[Value], key: &str) -> Option<usize> {
    if let Ok(n) = key.trim().parse::<usize>() {
        if n >= 1 && n <= items.len() {
            return Some(n - 1);
        }
    }
    let k = key.trim().to_lowercase();
    items
        .iter()
        .position(|i| i.get("text").and_then(Value::as_str).is_some_and(|t| t.to_lowercase() == k))
        .or_else(|| {
            items
                .iter()
                .position(|i| i.get("text").and_then(Value::as_str).is_some_and(|t| t.to_lowercase().contains(&k)))
        })
}

fn render_list(name: &str, items: &[Value]) -> String {
    let lines: Vec<String> = items
        .iter()
        .enumerate()
        .map(|(n, i)| {
            let done = i.get("done").and_then(Value::as_bool).unwrap_or(false);
            format!("{}. [{}] {}", n + 1, if done { "x" } else { " " }, i.get("text").and_then(Value::as_str).unwrap_or(""))
        })
        .collect();
    format!("{name}\n{}", lines.join("\n"))
}

fn parse_time(v: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    v.map(|t| DateTime::parse_from_rfc3339(t).map(|d| d.with_timezone(&Utc)).map_err(|_| anyhow!("{t} is not an RFC 3339 time")))
        .transpose()
}

fn render_todos(todos: &[Todo], agents: &[WorldAgent]) -> String {
    let now = Utc::now();
    let lines: Vec<String> = todos
        .iter()
        .enumerate()
        .map(|(n, t)| {
            let mut extra = vec![format!("{} old", age_label(now - t.added_at))];
            if t.priority != 2 {
                extra.push(PRIORITIES[t.priority.clamp(0, 3) as usize].to_string());
            }
            if let Some(a) = t.added_by_agent.and_then(|a| agents.iter().find(|x| x.id == a)) {
                extra.push(format!("added by {}", a.name));
            }
            if let Some(d) = t.due_at {
                extra.push(format!("due {} UTC", d.format("%Y-%m-%d %H:%M")));
            }
            if let Some(n) = t.nudge_at {
                extra.push(format!("next ping {} UTC", n.format("%Y-%m-%d %H:%M")));
            }
            if t.status == "open" && t.started_at.is_some() {
                extra.push("in progress".into());
            }
            if t.status == "dismissed" {
                extra.push("cancelled".into());
            }
            match (&t.decision, t.proposals.as_array().map(Vec::len).unwrap_or(0)) {
                (Some(d), _) => extra.push(format!("approved: {}", d.get("label").and_then(Value::as_str).unwrap_or("an action"))),
                (None, n) if n > 0 => extra.push(format!("{n} suggested action(s) waiting for approval")),
                _ => {}
            }
            let mark = if t.status == "open" { " " } else { "x" };
            format!("{}. [{mark}] {} ({}) {}", n + 1, t.text, t.short_id(), extra.join(", "))
        })
        .collect();
    format!("{}\n{}", loka_shared::todos::DEFAULT_LIST, lines.join("\n"))
}

fn proposal_note(todo: &Todo) -> String {
    match todo.proposals.as_array().map(Vec::len).unwrap_or(0) {
        0 => String::new(),
        n => format!(" They see {n} suggested action(s) on the card; when they approve one you get it as a message naming this to-do, then do exactly that and strike it off with complete_todo."),
    }
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, name: &str, args: &Value) -> Result<String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let owner = caller.owner_user_id;
    let region = match (arg("region"), owner) {
        (None, _) => None,
        (Some(key), _) if key.eq_ignore_ascii_case("hearth") => None,
        (Some(key), Some(owner)) => Some(store.find_region(owner, caller.org_id, key).await?.ok_or_else(|| anyhow!("no region called {key}; see list_regions"))?),
        (Some(_), None) => bail!("you have no person whose Loka you could build in"),
    };
    let region_id = region.as_ref().map(|r| r.id);
    let place = region.as_ref().map(|r| format!("in {}", r.name)).unwrap_or_else(|| "in the home town".into());
    let base = |kind: &'static str, item_name: String, data: Value, fires_at: Option<DateTime<Utc>>| async move {
        store
            .create_item(NewItem {
                kind,
                name: &item_name,
                owner_user_id: owner,
                created_by_agent: Some(caller.id),
                data,
                fires_at,
                region_id,
            })
            .await
    };
    match name {
        "place_building" => {
            let n = arg("name").ok_or_else(|| anyhow!("name is required"))?.to_string();
            let opens = match (args.get("opens").filter(|o| o.is_object()), owner) {
                (None, _) => Value::Null,
                (Some(o), Some(owner)) => store.resolve_opens(owner, o).await?,
                (Some(_), None) => bail!("only agents with a person can link buildings"),
            };
            let item = base(
                "building",
                n,
                json!({ "roof": hex(arg("roof")), "wall": hex(arg("wall")), "awning": hex(arg("awning")), "note": arg("note"), "opens": opens }),
                None,
            )
            .await?;
            let link = if opens.is_null() { String::new() } else { format!(" Walking in opens {} {}.", opens["kind"].as_str().unwrap_or(""), opens["ref"].as_str().unwrap_or("")) };
            Ok(format!("Built {} ({}) {place}. It appears on the map in a free spot.{link}", item.name, &item.id.to_string()[..8]))
        }
        "place_decoration" => {
            let kind = arg("kind").map(str::to_lowercase).ok_or_else(|| anyhow!("kind is required"))?;
            if !DECORATIONS.contains(&kind.as_str()) {
                bail!("kind must be one of: {}", DECORATIONS.join(", "));
            }
            let count = args.get("count").and_then(Value::as_i64).unwrap_or(1).clamp(1, 12);
            let label = arg("name").map(String::from).unwrap_or_else(|| format!("{count} {kind}"));
            let item = base("decoration", label, json!({ "sprite": kind, "count": count, "color": hex(arg("color")) }), None).await?;
            Ok(format!("Placed {} ({}) {place}.", item.name, &item.id.to_string()[..8]))
        }
        "add_critter" => {
            let n = arg("name").ok_or_else(|| anyhow!("name is required"))?.to_string();
            let species = arg("species").ok_or_else(|| anyhow!("species is required"))?;
            let item = base("critter", n, json!({ "species": species, "color": hex(arg("color")) }), None).await?;
            Ok(format!("{} the {} now roams {place}.", item.name, species))
        }
        "set_timer" => {
            let label = arg("label").ok_or_else(|| anyhow!("label is required"))?.to_string();
            let fires = match (args.get("minutes").and_then(Value::as_f64), arg("at")) {
                (Some(m), _) if m > 0.0 => Utc::now() + Duration::milliseconds((m * 60_000.0) as i64),
                (_, Some(at)) => DateTime::parse_from_rfc3339(at)?.with_timezone(&Utc),
                _ => bail!("give minutes or at"),
            };
            if fires <= Utc::now() {
                bail!("that time has already passed");
            }
            let then = arg("then").map(str::trim).filter(|t| !t.is_empty());
            if then.is_none() {
                let owner = owner.ok_or_else(|| anyhow!("you have no person to set a timer for"))?;
                let todo = store
                    .add_todo(NewTodo {
                        owner_user_id: owner,
                        text: label,
                        priority: None,
                        added_by_agent: Some(caller.id),
                        added_by_user: None,
                        source: None,
                        due_at: None,
                        nudge_at: Some(fires),
                        repeat_nudge: Some(false),
                        proposals: Vec::new(),
                    })
                    .await?;
                return Ok(format!(
                    "Timer \"{}\" ({}) set for {} UTC. It sits on their To do list and pings them once when it goes off.",
                    todo.text,
                    todo.short_id(),
                    fires.format("%Y-%m-%d %H:%M:%S")
                ));
            }
            let recent = store.reminders_set_since(caller.id, 24).await?;
            if recent >= REMINDERS_PER_DAY {
                bail!(
                    "You have set {recent} reminders in the last 24 hours. To check on something regularly, use create_schedule (or update your heartbeat) instead of setting a new reminder each time it fires."
                );
            }
            let item = base("timer", label, json!({ "then": then, "duration_ms": (fires - Utc::now()).num_milliseconds() }), Some(fires)).await?;
            Ok(format!("Reminder \"{}\" set for {} UTC. You will be woken to do it.", item.name, fires.format("%Y-%m-%d %H:%M:%S")))
        }
        "make_list" => {
            let title = arg("title").ok_or_else(|| anyhow!("title is required"))?.to_string();
            let note = note_style(args, &Value::Null)?;
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let org = caller.org_id.ok_or_else(|| anyhow!("you have no organization for the list"))?;
            let item = store
                .create_list(NewList {
                    owner,
                    creator: Some(caller.id),
                    org,
                    title: &title,
                    items: str_list(args.get("items")),
                    note: Some(note),
                    region: region_id,
                })
                .await?;
            Ok(format!("Made the list:\n{}", render_list(&item.name, item.data["items"].as_array().map(Vec::as_slice).unwrap_or(&[]))))
        }
        "update_list" => {
            let key = arg("list").ok_or_else(|| anyhow!("list is required"))?;
            let list = store.find_item(owner, caller.org_id, Some("list"), key).await?.ok_or_else(|| anyhow!("no list called {key}"))?;
            let mut items: Vec<Value> = list.data.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
            for t in str_list(args.get("add")) {
                items.push(json!({ "text": t, "done": false }));
            }
            for (field, done) in [("check", true), ("uncheck", false)] {
                for k in str_list(args.get(field)) {
                    if let Some(i) = match_index(&items, &k) {
                        items[i]["done"] = json!(done);
                    }
                }
            }
            let mut remove: Vec<usize> = str_list(args.get("remove")).iter().filter_map(|k| match_index(&items, k)).collect();
            remove.sort_unstable();
            remove.dedup();
            for i in remove.into_iter().rev() {
                items.remove(i);
            }
            let mut data = list.data.clone();
            data["items"] = json!(items);
            let note = note_style(args, list.data.get("note").unwrap_or(&Value::Null))?;
            if note.as_object().is_some_and(|n| !n.is_empty()) {
                data["note"] = note;
            }
            let updated = store.update_item(list.id, None, Some(&data), None).await?.ok_or_else(|| anyhow!("list vanished"))?;
            Ok(render_list(&updated.name, updated.data["items"].as_array().map(Vec::as_slice).unwrap_or(&[])))
        }
        "list_items" => {
            let filter = arg("kind");
            let items = store.items_in(owner, caller.org_id, false, region_id).await?;
            let lines: Vec<String> = items
                .into_iter()
                .filter(|i| filter.is_none_or(|k| i.kind == k))
                .map(|i| {
                    let extra = match i.kind.as_str() {
                        "npc" => i.data.get("agent_id").and_then(Value::as_str).map(|a| format!(" staffed by agent {}", &a[..a.len().min(8)])).unwrap_or_default(),
                        "timer" => i.fires_at.map(|f| format!(" fires {} UTC [{}]", f.format("%H:%M:%S"), i.status)).unwrap_or_default(),
                        "list" => {
                            let all = i.data.get("items").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                            let done = i.data.get("items").and_then(Value::as_array).map(|a| a.iter().filter(|x| x["done"] == json!(true)).count()).unwrap_or(0);
                            format!(" {done}/{all} done")
                        }
                        _ => String::new(),
                    };
                    format!("- {} {} ({}){}", i.kind, i.name, &i.id.to_string()[..8], extra)
                })
                .collect();
            Ok(if lines.is_empty() { format!("Nothing {place} yet.") } else { format!("{}:\n{}", place, lines.join("\n")) })
        }
        "remove_item" => {
            let key = arg("item").ok_or_else(|| anyhow!("item is required"))?;
            let item = store.find_item(owner, caller.org_id, None, key).await?.ok_or_else(|| anyhow!("nothing called {key}"))?;
            if item.owner_user_id != owner {
                bail!("{} belongs to someone else's Loka", item.name);
            }
            store.update_item(item.id, None, None, Some("archived")).await?;
            Ok(format!("Removed {} {}.", item.kind, item.name))
        }
        "edit_item" => {
            let key = arg("item").ok_or_else(|| anyhow!("item is required"))?;
            let item = store.find_item(owner, caller.org_id, None, key).await?.ok_or_else(|| anyhow!("nothing called {key}"))?;
            if owner.is_none() || item.owner_user_id != owner {
                bail!("{} belongs to someone else's Loka", item.name);
            }
            let owner_id = item.owner_user_id.unwrap_or_default();
            let mut data = item.data.as_object().cloned().unwrap_or_default();
            let mut new_name = arg("name").map(String::from);
            let mut changed: Vec<&str> = Vec::new();
            let set = |data: &mut serde_json::Map<String, Value>, k: &str, v: Value, changed: &mut Vec<&str>| {
                data.insert(k.to_string(), v);
                changed.push("details");
            };
            match item.kind.as_str() {
                "building" => {
                    for k in ["roof", "wall", "awning"] {
                        if let Some(c) = arg(k) {
                            set(&mut data, k, json!(hex(Some(c)).ok_or_else(|| anyhow!("{k} must be a hex colour like #a3e635"))?), &mut changed);
                        }
                    }
                    if let Some(n) = arg("note") {
                        set(&mut data, "note", json!(n), &mut changed);
                    }
                    match args.get("opens") {
                        Some(Value::Null) => set(&mut data, "opens", Value::Null, &mut changed),
                        Some(o) if o.is_object() => {
                            let opens = store.resolve_opens(owner_id, o).await?;
                            set(&mut data, "opens", opens, &mut changed);
                        }
                        _ => {}
                    }
                }
                "npc" => {
                    if let Some(p) = arg("persona") {
                        set(&mut data, "persona", json!(p.chars().take(1500).collect::<String>()), &mut changed);
                    }
                    if let Some(g) = arg("greeting") {
                        set(&mut data, "greeting", json!(g.chars().take(400).collect::<String>()), &mut changed);
                    }
                    if let Some(l) = args.get("look").filter(|l| l.is_object()) {
                        let mut look = data.get("look").and_then(Value::as_object).cloned().unwrap_or_default();
                        look.extend(l.as_object().cloned().unwrap_or_default());
                        set(&mut data, "look", Value::Object(look), &mut changed);
                    }
                    if args.get("dialogue").is_some() {
                        let dialogue = loka_shared::loka::parse_dialogue(args.get("dialogue"))?;
                        set(&mut data, "dialogue", json!(dialogue), &mut changed);
                    }
                    match args.get("agent") {
                        Some(Value::Null) => set(&mut data, "agent_id", Value::Null, &mut changed),
                        Some(Value::String(k)) if k.trim().is_empty() || k.trim().eq_ignore_ascii_case("none") => set(&mut data, "agent_id", Value::Null, &mut changed),
                        Some(Value::String(k)) => {
                            let staff = store.staff_agent(owner_id, k).await?;
                            set(&mut data, "agent_id", json!(staff.id), &mut changed);
                        }
                        _ => {}
                    }
                }
                "portal" => {
                    if let Some(l) = arg("label") {
                        set(&mut data, "label", json!(l), &mut changed);
                    }
                    if let Some(to) = arg("to") {
                        let dest = if to.eq_ignore_ascii_case("hearth") {
                            None
                        } else {
                            Some(store.find_region(owner_id, caller.org_id, to).await?.ok_or_else(|| anyhow!("no region called {to}; see list_regions"))?)
                        };
                        if dest.as_ref().map(|r| r.id) == item.region_id {
                            bail!("a portal has to lead somewhere else");
                        }
                        new_name.get_or_insert_with(|| dest.as_ref().map(|r| r.name.clone()).unwrap_or_else(|| "Home".into()));
                        set(&mut data, "to_region", json!(dest.as_ref().map(|r| r.id)), &mut changed);
                    }
                }
                "challenge" => {
                    let mut lesson_args = args.clone();
                    if let (Some(name), None) = (arg("name"), args.get("title")) {
                        lesson_args["title"] = json!(name);
                    }
                    if let Some((title, fresh)) = store.edit_lesson(&item, &lesson_args).await? {
                        data = fresh.as_object().cloned().unwrap_or_default();
                        new_name = Some(title);
                        changed.push("details");
                    }
                }
                "adventure" => {
                    if let Some(fresh) = store.edit_course(&item, args).await? {
                        data = fresh.as_object().cloned().unwrap_or_default();
                        changed.push("details");
                    }
                    if let Some(st) = args.get("story").and_then(Value::as_str) {
                        set(&mut data, "story", json!(st.trim().chars().take(4000).collect::<String>()), &mut changed);
                    }
                    if let Some(keys) = args.get("challenges").and_then(Value::as_array) {
                        let mut ids: Vec<uuid::Uuid> = Vec::new();
                        for k in keys.iter().filter_map(Value::as_str) {
                            let c = store
                                .find_item(Some(owner_id), caller.org_id, Some("challenge"), k)
                                .await?
                                .filter(|c| c.owner_user_id == Some(owner_id))
                                .ok_or_else(|| anyhow!("no challenge {k}"))?;
                            if !ids.contains(&c.id) {
                                ids.push(c.id);
                            }
                        }
                        if ids.is_empty() {
                            bail!("an adventure needs at least one challenge");
                        }
                        sqlx::query(
                            "UPDATE world_items SET data = data - 'adventure_id' - 'order', updated_at = NOW()
                             WHERE kind = 'challenge' AND data->>'adventure_id' = $1::text AND NOT (id = ANY($2))",
                        )
                        .bind(item.id)
                        .bind(&ids)
                        .execute(store.pool())
                        .await?;
                        for (n, id) in ids.iter().enumerate() {
                            sqlx::query("UPDATE world_items SET data = data || jsonb_build_object('adventure_id', $2::text, 'order', $3::int), updated_at = NOW() WHERE id = $1")
                                .bind(id)
                                .bind(item.id)
                                .bind(n as i32 + 1)
                                .execute(store.pool())
                                .await?;
                        }
                        set(&mut data, "challenges", json!(ids), &mut changed);
                    }
                }
                _ => {}
            }
            if changed.is_empty() && new_name.is_none() {
                bail!("nothing to change for a {}; see the edit_item description for its fields", item.kind);
            }
            let data = (!changed.is_empty()).then_some(Value::Object(data));
            let updated = store
                .update_item(item.id, new_name.as_deref(), data.as_ref(), None)
                .await?
                .ok_or_else(|| anyhow!("{} is gone", item.name))?;
            Ok(format!("Updated {} {} ({}).", updated.kind, updated.name, &updated.id.to_string()[..8]))
        }
        "add_todo" => {
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let text = arg("text").ok_or_else(|| anyhow!("text is required"))?.to_string();
            let nudge_at = match args.get("nudge_in_minutes").and_then(Value::as_f64) {
                Some(m) if m > 0.0 => Some(Utc::now() + Duration::milliseconds((m * 60_000.0) as i64)),
                _ => parse_time(arg("nudge_at"))?,
            };
            let suggested = args.get("suggested").and_then(Value::as_bool) == Some(true);
            let source = match (arg("source_url"), suggested) {
                (Some(u), true) => Some(json!({ "url": u, "label": arg("source_label"), "suggested": true })),
                (Some(u), false) => Some(json!({ "url": u, "label": arg("source_label") })),
                (None, true) => Some(json!({ "suggested": true })),
                (None, false) => None,
            };
            let todo = store
                .add_todo(NewTodo {
                    owner_user_id: owner,
                    text,
                    priority: priority_from(args.get("priority")),
                    added_by_agent: Some(caller.id),
                    added_by_user: None,
                    source,
                    due_at: parse_time(arg("due_at"))?,
                    nudge_at,
                    repeat_nudge: args.get("repeat").and_then(Value::as_bool),
                    proposals: args.get("actions").map(clean_proposals).unwrap_or_default(),
                })
                .await?;
            let ping = todo.nudge_at.map(|n| format!(" They get a ping at {} UTC.", n.format("%Y-%m-%d %H:%M"))).unwrap_or_default();
            let offered = proposal_note(&todo);
            Ok(format!("Added \"{}\" ({}) to their To do list.{ping}{offered}", todo.text, todo.short_id()))
        }
        "suggest_actions" => {
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let key = arg("todo").ok_or_else(|| anyhow!("todo is required"))?;
            let todo = store.find_todo(owner, key).await?.ok_or_else(|| anyhow!("no to-do matches {key}"))?;
            let proposals = args.get("actions").map(clean_proposals).unwrap_or_default();
            let by = TodoActor { agent: Some(caller.id), user: None };
            let todo = store.set_todo_proposals(todo.id, &proposals, by).await?.ok_or_else(|| anyhow!("that to-do is gone"))?;
            Ok(if proposals.is_empty() {
                format!("Cleared the suggested actions on \"{}\".", todo.text)
            } else {
                format!("Suggested {} action(s) on \"{}\" ({}).{}", proposals.len(), todo.text, todo.short_id(), proposal_note(&todo))
            })
        }
        "list_todos" => {
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let days = if args.get("include_done").and_then(Value::as_bool) == Some(true) { 7 } else { 0 };
            let todos = store.todos_for(owner, days).await?;
            if todos.is_empty() {
                return Ok("Their To do list is empty.".into());
            }
            Ok(render_todos(&todos, &store.list_agents().await?))
        }
        "complete_todo" | "snooze_todo" => {
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let key = arg("todo").ok_or_else(|| anyhow!("todo is required"))?;
            let todo = store.find_todo(owner, key).await?.ok_or_else(|| anyhow!("no to-do matches {key}"))?;
            let by = TodoActor { agent: Some(caller.id), user: None };
            if name == "complete_todo" {
                store.set_todo_status(todo.id, "done", by).await?;
                return Ok(format!("Struck off \"{}\".", todo.text));
            }
            let until = match (args.get("minutes").and_then(Value::as_f64), parse_time(arg("until"))?) {
                (Some(m), _) if m > 0.0 => Utc::now() + Duration::milliseconds((m * 60_000.0) as i64),
                (_, Some(at)) => at,
                _ => bail!("give minutes or until"),
            };
            store.snooze_todo(todo.id, until, by).await?;
            Ok(format!("Next ping for \"{}\" is {} UTC.", todo.text, until.format("%Y-%m-%d %H:%M")))
        }
        "update_todo" => {
            let owner = owner.ok_or_else(|| anyhow!("you have no person to keep a list for"))?;
            let key = arg("todo").ok_or_else(|| anyhow!("todo is required"))?;
            let todo = store.find_todo(owner, key).await?.ok_or_else(|| anyhow!("no to-do matches {key}"))?;
            let by = TodoActor { agent: Some(caller.id), user: None };
            let flag = |k: &str| args.get(k).and_then(Value::as_bool) == Some(true);
            if flag("drop") {
                store.set_todo_status(todo.id, "dismissed", by).await?;
                return Ok(format!("Dropped \"{}\" from the list.", todo.text));
            }
            let clearable = |k: &str| -> Result<Option<Option<DateTime<Utc>>>> {
                match arg(k) {
                    None => Ok(None),
                    Some(v) if v.eq_ignore_ascii_case("none") => Ok(Some(None)),
                    Some(v) => Ok(Some(parse_time(Some(v))?)),
                }
            };
            let patch = loka_shared::todos::TodoPatch {
                text: arg("text").map(String::from),
                priority: match args.get("priority") {
                    Some(v) => Some(priority_from(Some(v)).ok_or_else(|| anyhow!("priority must be one of {}", PRIORITIES.join(", ")))?),
                    None => None,
                },
                due_at: clearable("due_at")?,
                nudge_at: clearable("nudge_at")?,
                repeat_nudge: None,
                started: None,
            };
            let reopen = flag("reopen");
            let position = args.get("position").and_then(Value::as_i64);
            if patch.text.is_none() && patch.priority.is_none() && patch.due_at.is_none() && patch.nudge_at.is_none() && !reopen && position.is_none() {
                bail!("give text, priority, due_at, nudge_at, position, drop or reopen");
            }
            if reopen {
                store.set_todo_status(todo.id, "open", by).await?;
            }
            let updated = store.update_todo(todo.id, &patch, by).await?.unwrap_or(todo);
            if let Some(position) = position {
                let open: Vec<Uuid> = store.todos_for(owner, 0).await?.into_iter().filter(|t| t.status == "open").map(|t| t.id).collect();
                store.reorder_todos(owner, &move_to_position(&open, updated.id, position)).await?;
            }
            Ok(format!("Updated \"{}\".", updated.text))
        }
        other => bail!("unknown tool {other}"),
    }
}

pub const NOTE_COLORS: &[&str] = &["lemon", "mint", "rose", "aqua", "honey", "seafoam", "coral", "sand"];
pub const NOTE_SIZES: &[&str] = &["small", "medium", "large"];

fn move_to_position(order: &[Uuid], id: Uuid, position: i64) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = order.iter().copied().filter(|x| *x != id).collect();
    let at = (position.max(1) as usize - 1).min(ids.len());
    ids.insert(at, id);
    ids
}

fn note_style(args: &Value, current: &Value) -> Result<Value> {
    let mut note = current.as_object().cloned().unwrap_or_default();
    if let Some(color) = args.get("color").and_then(Value::as_str) {
        let color = color.trim().to_ascii_lowercase();
        if !NOTE_COLORS.contains(&color.as_str()) {
            bail!("color must be one of {}", NOTE_COLORS.join(", "));
        }
        note.insert("color".into(), json!(color));
    }
    if let Some(size) = args.get("size").and_then(Value::as_str) {
        let size = size.trim().to_ascii_lowercase();
        if !NOTE_SIZES.contains(&size.as_str()) {
            bail!("size must be one of {}", NOTE_SIZES.join(", "));
        }
        note.insert("size".into(), json!(size));
    }
    if let Some(position) = args.get("position").and_then(Value::as_i64) {
        note.insert("order".into(), json!(position.clamp(1, 999)));
    }
    Ok(Value::Object(note))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_a_todo_to_a_position() {
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        assert_eq!(move_to_position(&ids, ids[3], 1), vec![ids[3], ids[0], ids[1], ids[2]]);
        assert_eq!(move_to_position(&ids, ids[0], 3), vec![ids[1], ids[2], ids[0], ids[3]]);
        assert_eq!(move_to_position(&ids, ids[1], 99), vec![ids[0], ids[2], ids[3], ids[1]]);
        assert_eq!(move_to_position(&ids, ids[2], 0), vec![ids[2], ids[0], ids[1], ids[3]]);
        let fresh = Uuid::new_v4();
        assert_eq!(move_to_position(&ids[..1], fresh, 1), vec![fresh, ids[0]]);
    }
}
