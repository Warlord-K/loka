use anyhow::{anyhow, bail, Result};
use loka_shared::academy;
use loka_shared::items::NewItem;
use loka_shared::loka::{self, Region, RegionPatch};
use loka_shared::world::{WorldAgent, WorldStore};
use serde_json::{json, Value};
use uuid::Uuid;

pub const BUILD_TOOLS: &[&str] = &["create_region", "update_region", "place_portal", "add_npc", "set_ambient"];
pub const ACADEMY_TOOLS: &[&str] = &["create_challenge", "create_course", "create_adventure"];
pub const LOOK_TOOLS: &[&str] = &["list_regions", "list_challenges", "progress_of"];

pub fn look_tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    vec![
        tool(
            "list_regions",
            "List the regions of your person's Loka (beyond the home town, their home region): name, slug, theme, visibility, ambient and how many places each holds. archived lists the archived ones instead, which update_region can bring back.",
            json!({ "archived": { "type": "boolean", "description": "List archived regions instead" } }),
            &[],
        ),
        tool(
            "list_challenges",
            "List your person's Academy courses and their lessons (challenges), with how they did on each (new, pending, passed, failed) and their best score. Give course to list one course in order, challenge to read one lesson in full (prompt, reading, coach, attempts), and for a practice workspace lesson attempt (practice:<id>) with strokes true for the stroke data of that attempt.",
            json!({
                "state": { "type": "string", "description": "Only new, pending, passed or failed, optional" },
                "course": { "type": "string", "description": "Only the lessons of this course: name or id, optional" },
                "challenge": { "type": "string", "description": "One lesson to read in full: name or id, optional" },
                "attempt": { "type": "string", "description": "A practice workspace attempt to read in full: practice:<id> or its first characters, optional" },
                "strokes": { "type": "boolean", "description": "With attempt or a practice workspace challenge: include the full stroke JSON" }
            }),
            &[],
        ),
        tool("progress_of", "See your person's Academy progress: xp, level, per-skill xp, streak, unlocked rewards and recent graded attempts. Use it to pitch the next challenge at the right level and skill.", json!({}), &[]),
    ]
}

pub fn build_tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let region = s("Region name, slug or id from list_regions; omit for the home town");
    vec![
        tool(
            "create_region",
            "Grow your person's Loka with a new region beyond the home town. A portal to it appears in the home town and a portal back appears inside it. Build into it with place_building, place_decoration, add_critter, add_npc and place_portal using its name as region.",
            json!({
                "name": s("Region name, like Whispering Pines"),
                "theme": { "type": "string", "enum": loka::THEMES },
                "description": s("What this place is for, one or two sentences, optional"),
                "visibility": { "type": "string", "enum": ["private", "public"], "description": "private (default): only your person sees it; public: everyone can visit" }
            }),
            &["name"],
        ),
        tool(
            "update_region",
            "Rename a region, change its theme, description or visibility, archive it (its portals close), or bring an archived one back with archived false (its portals reopen).",
            json!({
                "region": s("Region name, slug or id"),
                "name": s("New name, optional"),
                "theme": { "type": "string", "enum": loka::THEMES },
                "description": s("New description, optional"),
                "visibility": { "type": "string", "enum": ["private", "public"] },
                "archived": { "type": "boolean" }
            }),
            &["region"],
        ),
        tool(
            "place_portal",
            "Place a portal that takes people from one place to another: from the home town or a region, to another region or back to the home town.",
            json!({
                "to": s("Destination region name, slug or id, or hearth"),
                "region": region.clone(),
                "label": s("Sign text, optional")
            }),
            &["to"],
        ),
        tool(
            "add_npc",
            "Add a character who lives in the home town or a region. People can talk to them: a cheap, tool-less conversation in the character's voice runs on your person's keys. Give a persona and greeting; dialogue is an optional scripted conversation the app shows without any model call. Set agent to staff the character with one of your person's own agents: talking to it then sends that agent a real chat message, so it can answer and start quests, and the character wears that agent's look.",
            json!({
                "name": s("Character name"),
                "region": region.clone(),
                "agent": s("Handle, name or id of one of your person's agents who staffs this character, optional"),
                "persona": s("Who they are and how they speak, a few sentences"),
                "greeting": s("First thing they say"),
                "look": { "type": "object", "description": super::mcp::look_guide("Sprite hints like the agents' look, optional.") },
                "dialogue": { "type": "array", "description": "Optional script: lines with id, text and up to 4 options {label, next} where next is another line id", "items": { "type": "object" } }
            }),
            &["name"],
        ),
        tool(
            "set_ambient",
            "Set the mood of a region: weather, time of day, ambient critters and music.",
            json!({
                "region": s("Region name, slug or id"),
                "weather": { "type": "string", "enum": loka::WEATHERS },
                "time_of_day": { "type": "string", "enum": loka::TIMES_OF_DAY },
                "critters": { "type": "array", "description": "Ambient wildlife, like [{\"species\": \"firefly\", \"count\": 6}]", "items": { "type": "object" } },
                "music": s("Music mood, a few words, optional")
            }),
            &["region"],
        ),
    ]
}

pub fn academy_tools(tool: fn(&str, &str, Value, &[&str]) -> Value) -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let region = s("Region name, slug or id from list_regions; omit for the home town");
    vec![
        tool(
            "create_challenge",
            "Create an Academy lesson (challenge) for your person: a puzzle, dsa (data structures and algorithms), design (system design), quiz, or canvas (shown as a practice workspace, where they write or draw and press Ready; the coach grades the picture and strokes with grade_attempt). Puzzle, dsa, design and quiz are answered in the app: quizzes with choices and correct are graded instantly, the rest by a short examiner conversation using your rubric or tests. Passing earns xp in the skill. Give course to add it to a course in order, reading for the reports or workspaces to read first, coach for who grades it. Keep it doable in the minutes you give. When your person is waiting on a quest, relate it to that quest's subject.",
            json!({
                "title": s("Short title"),
                "type": { "type": "string", "enum": academy::TYPES },
                "prompt": s("The full challenge text the person reads; markdown is fine"),
                "skill": s(&format!("Skill it trains, e.g. {}", academy::SKILLS.join(", "))),
                "difficulty": { "type": "string", "enum": academy::DIFFICULTIES },
                "minutes": { "type": "integer", "description": "Expected minutes, 2 to 120, default 10" },
                "xp": { "type": "integer", "description": "XP for passing, 5 to 200; defaults 20 easy, 40 medium, 80 hard" },
                "hints": { "type": "array", "items": { "type": "string" }, "description": "Up to 6 hints, gentlest first" },
                "rubric": s("What a passing answer must contain; private, never shown to the person"),
                "tests": { "type": "object", "description": "For dsa: {language, harness} with the test cases the examiner traces the answer against", "properties": { "language": { "type": "string" }, "harness": { "type": "string" } } },
                "choices": { "type": "array", "items": { "type": "string" }, "description": "For a multiple choice quiz" },
                "correct": { "type": "array", "items": { "type": "integer" }, "description": "1-based positions of the right choices; private" },
                "course": s("Course name or id to add this lesson to, in order; a practice workspace lesson without one joins the course of its skill"),
                "reading": { "type": "array", "items": { "type": "string" }, "description": "Up to 12 of your person's reports or workspaces to read first: report:<id>, id, title or slug" },
                "coach": s("Agent name or slug that grades it; default the course coach, then the last coach for the skill, then the main agent"),
                "region": region.clone(),
                "task_id": s("The quest this challenge relates to, optional")
            }),
            &["title", "type", "skill"],
        ),
        tool(
            "create_course",
            "Make an Academy course for your person: a topic with an ordered path of lessons (challenges). Add lessons now with challenges, or later with create_challenge course. Finishing every lesson earns a trophy for their home town and wakes the coach to plan what comes next.",
            json!({
                "title": s("Course title, the topic"),
                "story": s("A few sentences on what the course covers and why"),
                "challenges": { "type": "array", "items": { "type": "string" }, "description": "Existing lesson names or ids, in order, optional" },
                "skill": s("Skill it trains, optional"),
                "folder": s("Reports folder where its workspaces and reading live, optional"),
                "coach": s("Agent name or slug that coaches the whole course, optional"),
                "region": region
            }),
            &["title"],
        ),
    ]
}

fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

async fn region_arg(store: &WorldStore, owner: Uuid, org: Option<Uuid>, key: Option<&str>) -> Result<Option<Region>> {
    match key {
        None => Ok(None),
        Some(k) if k.eq_ignore_ascii_case("hearth") => Ok(None),
        Some(k) => Ok(Some(store.find_region(owner, org, k).await?.ok_or_else(|| anyhow!("no region called {k}; see list_regions"))?)),
    }
}

fn place(region: Option<&Region>) -> String {
    region.map(|r| format!("in {}", r.name)).unwrap_or_else(|| "in the home town".into())
}

pub async fn call(store: &WorldStore, caller: &WorldAgent, name: &str, args: &Value) -> Result<String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let owner = caller.owner_user_id.ok_or_else(|| anyhow!("you have no person whose Loka you could shape"))?;
    let me = Some(caller.id);
    match name {
        "list_regions" if args.get("archived").and_then(Value::as_bool).unwrap_or(false) => {
            let regions = store.archived_regions(owner, caller.org_id).await?;
            if regions.is_empty() {
                return Ok("No archived regions.".into());
            }
            Ok(regions
                .iter()
                .map(|r| format!("- {} ({}, slug {}) {} {}, archived {}", r.name, short(r.id), r.slug, r.theme, r.visibility, r.updated_at.format("%Y-%m-%d")))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "list_regions" => {
            let regions: Vec<Region> = store.regions_for(owner, caller.org_id, false).await?.into_iter().filter(|r| r.owner_user_id == owner).collect();
            if regions.is_empty() {
                return Ok("Only the home town so far. Grow Loka with create_region.".into());
            }
            let mut lines = Vec::new();
            for r in regions {
                let places = store.items_in(Some(owner), caller.org_id, false, Some(r.id)).await?.len();
                let ambient = r.data.get("ambient").map(|a| format!(" ambient {a}")).unwrap_or_default();
                lines.push(format!("- {} ({}, slug {}) {} {}, {places} places{ambient}", r.name, short(r.id), r.slug, r.theme, r.visibility));
            }
            Ok(lines.join("\n"))
        }
        "list_challenges" if arg("attempt").is_some() => {
            let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
            super::practice_tools::read(store, owner, org, None, arg("attempt"), args.get("strokes").and_then(Value::as_bool).unwrap_or(false)).await
        }
        "list_challenges" if arg("challenge").is_some() => {
            let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
            let key = arg("challenge").unwrap_or_default();
            let item = store.find_item(Some(owner), Some(org), Some("challenge"), key).await?.filter(|i| i.owner_user_id == Some(owner)).ok_or_else(|| anyhow!("no lesson called {key}"))?;
            let d = store.challenge_detail(owner, Some(org), item.id).await?.ok_or_else(|| anyhow!("no lesson called {key}"))?;
            let c = &d.card;
            let mut out = format!("{} ({}) {} {} {}, {} xp, ~{} min [{}]
{}", c.title, short(c.id), c.challenge_type, c.skill, c.difficulty, c.xp, c.minutes, c.state, d.prompt);
            if !d.reading.is_empty() {
                out.push_str(&format!("
Reading: {}", d.reading.iter().map(|r| format!("report:{} \"{}\"", r.id, r.title)).collect::<Vec<_>>().join(", ")));
            }
            out.push_str(&format!("
Coach: {}", d.coach.as_ref().map(|k| format!("{} (@{})", k.name, k.slug)).unwrap_or_else(|| "nobody yet".into())));
            if let Some(report) = c.report_id {
                out.push_str(&format!("

{}", super::practice_tools::read(store, owner, org, Some(&report.to_string()), None, args.get("strokes").and_then(Value::as_bool).unwrap_or(false)).await?));
            } else if !d.attempts.is_empty() {
                out.push_str("
Attempts:");
                for a in &d.attempts {
                    out.push_str(&format!("
- {} {}{} {}", short(a.id), a.status, a.score.map(|s| format!(" ({s})")).unwrap_or_default(), a.feedback.as_deref().map(|f| loka_shared::links::excerpt(f, 160)).unwrap_or_default()));
                }
            }
            Ok(out)
        }
        "list_challenges" => {
            let state = arg("state");
            let course = match arg("course") {
                Some(key) => Some(store.find_course(owner, caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?, key).await?),
                None => None,
            };
            let mut cards = store.challenge_cards(owner, caller.org_id).await?;
            if let Some(c) = &course {
                cards.retain(|card| card.adventure_id == Some(c.id));
                cards.sort_by_key(|card| card.adventure_order.unwrap_or(i32::MAX));
            }
            let lines: Vec<String> = cards
                .iter()
                .filter(|c| state.is_none_or(|s| c.state == s))
                .map(|c| {
                    let best = c.best_score.map(|s| format!(", best {s}")).unwrap_or_default();
                    format!("- challenge:{} {} {} {} {}, {} xp, ~{} min [{}{best}]", c.id, c.title, c.challenge_type, c.skill, c.difficulty, c.xp, c.minutes, c.state)
                })
                .collect();
            let adventures: Vec<_> = store.adventures_for(owner, caller.org_id).await?.into_iter().filter(|a| course.as_ref().is_none_or(|c| c.id == a.id)).collect();
            let mut out = if lines.is_empty() { "No lessons yet.".to_string() } else { lines.join("\n") };
            if !adventures.is_empty() {
                out.push_str("\nCourses:");
                for a in adventures {
                    let skill = a.skill.as_deref().map(|s| format!(" {s}")).unwrap_or_default();
                    out.push_str(&format!("\n- course:{} {}{skill} {}/{} passed{}", a.id, a.title, a.passed, a.total, if a.completed { ", completed" } else { "" }));
                }
            }
            Ok(out)
        }
        "progress_of" => {
            let p = store.progress(owner).await?;
            let unlocks: Vec<String> = p.unlocks.as_array().map(|a| a.iter().filter_map(|u| u.get("name").and_then(Value::as_str).map(String::from)).collect()).unwrap_or_default();
            let recent: Vec<String> = sqlx::query_as::<_, (String, String, Option<i32>)>(
                "SELECT i.name, a.status, a.score FROM challenge_attempts a JOIN world_items i ON i.id = a.challenge_id
                 WHERE a.user_id = $1 ORDER BY a.created_at DESC LIMIT 8",
            )
            .bind(owner)
            .fetch_all(store.pool())
            .await?
            .into_iter()
            .map(|(t, s, sc)| format!("- {t}: {s}{}", sc.map(|v| format!(" ({v})")).unwrap_or_default()))
            .collect();
            Ok(format!(
                "Level {} with {} xp ({} to next level). Skills: {}. Streak {} days (best {}). Solved {} of {} attempts. Unlocked: {}.\nRecent attempts:\n{}",
                p.level,
                p.xp,
                p.next_level_xp - p.xp,
                p.skills,
                p.streak,
                p.best_streak,
                p.solved,
                p.attempts,
                if unlocks.is_empty() { "nothing yet".into() } else { unlocks.join(", ") },
                if recent.is_empty() { "(none yet)".into() } else { recent.join("\n") }
            ))
        }
        "create_region" => {
            let name = arg("name").ok_or_else(|| anyhow!("name is required"))?;
            let r = store.create_region(owner, name, arg("theme"), arg("visibility"), arg("description"), me).await?;
            Ok(format!("Created the {} region {} ({}, slug {}). Portals connect it to the home town. Build into it with region \"{}\".", r.theme, r.name, short(r.id), r.slug, r.slug))
        }
        "update_region" => {
            let key = arg("region").ok_or_else(|| anyhow!("region is required"))?;
            let r = store.find_region_any(owner, caller.org_id, key).await?.ok_or_else(|| anyhow!("no region called {key}; see list_regions"))?;
            let patch = RegionPatch {
                name: arg("name").map(String::from),
                theme: arg("theme").map(String::from),
                visibility: arg("visibility").map(String::from),
                data: arg("description").map(|d| json!({ "description": d.chars().take(600).collect::<String>() })),
                archived: args.get("archived").and_then(Value::as_bool),
            };
            let r = store.update_region(r.id, &patch, me).await?.ok_or_else(|| anyhow!("that region is gone"))?;
            Ok(format!("{} is now {} {} [{}].", r.name, r.theme, r.visibility, r.status))
        }
        "set_ambient" => {
            let key = arg("region").ok_or_else(|| anyhow!("region is required"))?;
            let r = store.find_region(owner, caller.org_id, key).await?.ok_or_else(|| anyhow!("no region called {key}"))?;
            let r = store.set_region_ambient(r.id, args, me).await?;
            Ok(format!("{} ambient: {}", r.name, r.data.get("ambient").cloned().unwrap_or(Value::Null)))
        }
        "place_portal" => {
            let to_key = arg("to").ok_or_else(|| anyhow!("to is required"))?;
            let to = region_arg(store, owner, caller.org_id, Some(to_key)).await?;
            let from = region_arg(store, owner, caller.org_id, arg("region")).await?;
            if from.as_ref().map(|r| r.id) == to.as_ref().map(|r| r.id) {
                bail!("a portal has to lead somewhere else");
            }
            let dest = to.as_ref().map(|r| r.name.clone()).unwrap_or_else(|| "Home".into());
            let label = arg("label").map(String::from).unwrap_or_else(|| format!("To {dest}"));
            let item = store
                .create_item(NewItem {
                    kind: "portal",
                    name: &dest,
                    owner_user_id: Some(owner),
                    created_by_agent: me,
                    data: json!({ "to_region": to.as_ref().map(|r| r.id), "label": label }),
                    fires_at: None,
                    region_id: from.as_ref().map(|r| r.id),
                })
                .await?;
            Ok(format!("Placed a portal to {dest} {} ({}).", place(from.as_ref()), short(item.id)))
        }
        "add_npc" => {
            let n = arg("name").ok_or_else(|| anyhow!("name is required"))?;
            let staff = match arg("agent") {
                Some(key) => Some(store.staff_agent(owner, key).await?),
                None => None,
            };
            let fallback = staff.as_ref().map(|a| format!("{} stands in here for {}.", n, a.name));
            let persona = arg("persona").or(fallback.as_deref()).ok_or_else(|| anyhow!("persona is required unless agent staffs the character"))?;
            let region = region_arg(store, owner, caller.org_id, arg("region")).await?;
            let dialogue = loka::parse_dialogue(args.get("dialogue"))?;
            let look = args.get("look").filter(|l| l.is_object()).cloned().unwrap_or_else(|| json!({}));
            let item = store
                .create_item(NewItem {
                    kind: "npc",
                    name: n,
                    owner_user_id: Some(owner),
                    created_by_agent: me,
                    data: json!({
                        "persona": persona.chars().take(1500).collect::<String>(),
                        "greeting": arg("greeting").map(|g| g.chars().take(400).collect::<String>()),
                        "look": look,
                        "dialogue": dialogue,
                        "agent_id": staff.as_ref().map(|a| a.id),
                    }),
                    fires_at: None,
                    region_id: region.as_ref().map(|r| r.id),
                })
                .await?;
            let by = staff.as_ref().map(|a| format!(" Staffed by {}: talking to them sends {} a real message.", a.name, a.name)).unwrap_or_default();
            Ok(format!("{} now lives {} ({}). Your person can talk to them on the map.{by}", item.name, place(region.as_ref()), short(item.id)))
        }
        "create_challenge" => {
            let region = region_arg(store, owner, caller.org_id, arg("region")).await?;
            let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
            let (item, offer) = store.create_challenge(owner, org, me, args, region.as_ref().map(|r| r.id)).await?;
            let offered = if offer.is_some() { " It answers their open while-you-wait offer, so they see it now." } else { "" };
            Ok(format!(
                "Created the {} lesson {}, {} xp, about {} minutes.{offered} Put challenge:{} on its own line in chat so your person sees it as a card they open in the Academy.",
                item.data["type"].as_str().unwrap_or(""),
                item.name,
                item.data["xp"],
                item.data["minutes"],
                item.id
            ))
        }
        "create_course" | "create_adventure" => {
            let org = caller.org_id.ok_or_else(|| anyhow!("you are not in an organization"))?;
            let region = region_arg(store, owner, caller.org_id, arg("region")).await?;
            let item = store.create_course(owner, org, me, args, region.as_ref().map(|r| r.id)).await?;
            let lessons = item.data.get("challenges").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            Ok(format!("Created the course {} with {lessons} lessons. Add more with create_challenge course \"{}\". Put course:{} on its own line in chat so your person sees it as a card.", item.name, item.name, item.id))
        }
        other => bail!("unknown tool {other}"),
    }
}
