use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::reports::{Report, ReportActor};
use crate::world::{NewEvent, WorldStore};

pub const MAX_ITEMS: usize = 400;
pub const MAX_EDGES: usize = 800;
pub const MAX_TEXT: usize = 8000;
pub const MAX_COMMAND: usize = 2000;
pub const MAX_LABEL: usize = 200;
pub const MAX_INK_POINTS: usize = 2000;
pub const MAX_BYTES: usize = 480 * 1024;
pub const MAX_MARK: usize = 2000;
pub const MAX_OPEN_MARKS: i64 = 200;
const MAX_COORD: f64 = 1_000_000.0;
const MIN_SIZE: f64 = 24.0;
const MAX_SIZE: f64 = 4000.0;
const GAP: f64 = 48.0;

pub const KINDS: [&str; 10] = ["image", "video", "model", "note", "sticky", "ref", "run", "frame", "ink", "ui"];
pub const COLORS: [&str; 6] = ["lemon", "rose", "mint", "aqua", "honey", "sand"];
pub const LAYOUTS: [&str; 3] = ["row", "grid", "flow"];
pub const MODEL_MIME: &str = "model/gltf-binary";
const REF_KINDS: [&str; 6] = ["quest", "report", "msg", "todo", "agent", "practice"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Value>,
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemInput {
    pub id: Option<String>,
    pub kind: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub w: Option<f64>,
    pub h: Option<f64>,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub upload_id: Option<Uuid>,
    pub target: Option<String>,
    pub command: Option<String>,
    pub color: Option<String>,
    pub locked: Option<bool>,
    pub points: Option<Vec<[f64; 2]>>,
    pub spec: Option<Value>,
    pub by: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EdgeInput {
    pub id: Option<String>,
    pub from: String,
    pub to: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Add {
        #[serde(default)]
        items: Vec<ItemInput>,
        #[serde(default)]
        edges: Vec<EdgeInput>,
    },
    Update {
        items: Vec<ItemInput>,
    },
    Remove {
        ids: Vec<String>,
    },
    Connect {
        edges: Vec<EdgeInput>,
    },
    Disconnect {
        ids: Vec<String>,
    },
    Layout {
        ids: Vec<String>,
        mode: String,
        #[serde(default)]
        gap: Option<f64>,
        #[serde(default)]
        columns: Option<usize>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
    },
}

impl Op {
    fn reshapes(&self) -> bool {
        matches!(self, Op::Add { .. } | Op::Remove { .. } | Op::Connect { .. } | Op::Disconnect { .. })
            || matches!(self, Op::Update { items } if items.iter().any(|i| i.text.is_some() || i.caption.is_some() || i.upload_id.is_some() || i.command.is_some() || i.target.is_some() || i.spec.is_some()))
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Mark {
    pub id: Uuid,
    pub report_id: Uuid,
    pub item_id: Option<String>,
    pub x: f64,
    pub y: f64,
    pub kind: String,
    pub body: String,
    pub status: String,
    pub reply: Option<String>,
    pub author_user_id: Option<Uuid>,
    pub resolved_by_agent_id: Option<Uuid>,
    pub resolved_by_user_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewMark {
    pub item_id: Option<String>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub kind: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct CanvasHit {
    pub id: Uuid,
    pub title: String,
    pub folder: String,
    pub updated_at: DateTime<Utc>,
    pub items: Value,
}

pub fn item_hits_sql(pattern: &str) -> String {
    format!(
        "COALESCE((SELECT jsonb_agg(jsonb_build_object('id', found.i->>'id', 'kind', found.i->>'kind',
             'label', left(regexp_replace(COALESCE(found.i->>'caption', found.i->>'text', found.i->>'target', found.i->>'command', found.card, 'ui card'), '\\s+', ' ', 'g'), 160)))
           FROM (SELECT x.i, x.card FROM (
                   SELECT value AS i,
                          CASE WHEN value->>'kind' = 'ui' THEN (SELECT string_agg(s #>> '{{}}', ' ') FROM jsonb_path_query(value->'spec', '{UI_TEXT_PATH}', '{{}}', true) s) END AS card
                   FROM jsonb_array_elements(CASE WHEN r.kind = 'canvas' AND jsonb_typeof(r.canvas->'items') = 'array' THEN r.canvas->'items' ELSE '[]'::jsonb END)) x
                 WHERE x.i->>'kind' <> 'ink'
                   AND concat_ws(' ', x.i->>'text', x.i->>'caption', x.i->>'target', x.i->>'command', x.card) ILIKE {pattern}
                 LIMIT 3) found), '[]'::jsonb)"
    )
}

const UI_TEXT_PATH: &str = "strict $.elements.*.props.** ? (@.type() == \"string\")";

pub fn new_id(prefix: char) -> String {
    format!("{prefix}{}", &Uuid::new_v4().simple().to_string()[..8])
}

fn clean_id(raw: &str) -> Result<String> {
    let id = raw.trim();
    if id.is_empty() || id.len() > 40 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("item and arrow ids are 1 to 40 letters, digits, - or _");
    }
    Ok(id.to_string())
}

fn default_size(kind: &str) -> (f64, f64) {
    match kind {
        "image" => (320.0, 240.0),
        "video" => (360.0, 240.0),
        "model" => (320.0, 320.0),
        "sticky" => (220.0, 200.0),
        "ref" => (300.0, 96.0),
        "run" => (380.0, 150.0),
        "frame" => (720.0, 420.0),
        "ui" => (360.0, 280.0),
        _ => (320.0, 200.0),
    }
}

fn text_field(value: Option<String>, max: usize, what: &str) -> Result<Option<String>> {
    let Some(v) = value else { return Ok(None) };
    let v = v.replace('\u{2014}', " - ");
    if v.chars().count() > max {
        bail!("{what} is at most {max} characters");
    }
    Ok(if v.trim().is_empty() { None } else { Some(v) })
}

pub fn ref_ok(target: &str) -> bool {
    let Some((kind, id)) = target.split_once(':') else { return false };
    REF_KINDS.contains(&kind) && (3..=80).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Canvas {
    pub fn parse(value: Option<&Value>) -> Result<Canvas> {
        match value {
            None | Some(Value::Null) => Ok(Canvas::default()),
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| anyhow!("unreadable canvas: {e}")),
        }
    }

    pub fn from_raw(value: &Value, by: &str) -> Result<Canvas> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            items: Vec<ItemInput>,
            #[serde(default)]
            edges: Vec<EdgeInput>,
        }
        let raw: Raw = serde_json::from_value(value.clone()).map_err(|e| anyhow!("unreadable canvas: {e}"))?;
        if raw.items.len() > MAX_ITEMS {
            bail!("a canvas holds at most {MAX_ITEMS} items; start another canvas and link them");
        }
        let mut taken: HashSet<String> = raw.items.iter().filter_map(|i| i.id.as_deref().map(|id| id.trim().to_string())).collect();
        let items: Vec<ItemInput> = raw
            .items
            .into_iter()
            .map(|mut input| {
                if input.id.is_none() {
                    input.id = Some(loop {
                        let id = new_id('i');
                        if taken.insert(id.clone()) {
                            break id;
                        }
                    });
                }
                input.by = None;
                if let (Some(points), Some(x), Some(y)) = (input.points.as_mut(), input.x, input.y) {
                    points.iter_mut().for_each(|p| *p = [p[0] + x, p[1] + y]);
                }
                input
            })
            .collect();
        let order: HashMap<String, usize> = items.iter().enumerate().filter_map(|(n, i)| i.id.as_deref().map(|id| (id.trim().to_string(), n))).collect();
        let mut canvas = Canvas::default();
        canvas.apply_ops(vec![Op::Add { items, edges: raw.edges }], by)?;
        canvas.items.sort_by_key(|i| order.get(&i.id).copied().unwrap_or(usize::MAX));
        canvas.validate()?;
        Ok(canvas)
    }

    pub fn near_item(&self, sticky: Option<&str>, at: (f64, f64)) -> Option<String> {
        let area = match sticky {
            Some(id) => {
                let host = self.items.iter().find(|i| i.id == id)?;
                if host.kind != "sticky" {
                    return Some(host.id.clone());
                }
                (host.x, host.y, host.x + host.w, host.y + host.h)
            }
            None => (at.0, at.1, at.0, at.1),
        };
        let under = |i: &Item, (x0, y0, x1, y1): (f64, f64, f64, f64)| i.x <= x1 && x0 <= i.x + i.w && i.y <= y1 && y0 <= i.y + i.h;
        let tests = [(area.0, area.1, area.0, area.1), area];
        tests.iter().flat_map(|&zone| [false, true].map(move |frames| (zone, frames))).find_map(|(zone, frames)| {
            self.items
                .iter()
                .rev()
                .filter(|i| Some(i.id.as_str()) != sticky && !matches!(i.kind.as_str(), "sticky" | "ink") && (i.kind == "frame") == frames)
                .find(|i| under(i, zone))
                .map(|i| i.id.clone())
        })
    }

    pub fn sticky_of<'a>(&self, item: Option<&'a str>) -> Option<&'a str> {
        item.filter(|id| self.items.iter().any(|i| i.id == *id && i.kind == "sticky"))
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({ "items": [], "edges": [] }))
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        self.items.iter().fold(None, |acc, i| {
            let (x0, y0, x1, y1) = acc.unwrap_or((i.x, i.y, i.x + i.w, i.y + i.h));
            Some((x0.min(i.x), y0.min(i.y), x1.max(i.x + i.w), y1.max(i.y + i.h)))
        })
    }

    pub fn upload_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.items.iter().filter_map(|i| i.upload_id).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn item_line(&self, id: &str) -> Option<String> {
        let item = self.items.iter().find(|i| i.id == id)?;
        let spec = item.spec.as_ref().map(spec_text);
        let label = item.caption.as_deref().or(item.text.as_deref()).or(item.target.as_deref()).or(item.command.as_deref()).or(spec.as_deref()).unwrap_or("");
        let label: String = label.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect();
        Some(if label.is_empty() { format!("{} {}", item.kind, item.id) } else { format!("{} {}: {label}", item.kind, item.id) })
    }

    fn fill(&self, input: ItemInput, by: &str, taken: &mut HashSet<String>) -> Result<Item> {
        let kind = input.kind.as_deref().map(str::trim).unwrap_or("").to_lowercase();
        if !KINDS.contains(&kind.as_str()) {
            bail!("kind is one of {}", KINDS.join(", "));
        }
        let id = match input.id.as_deref() {
            Some(raw) => clean_id(raw)?,
            None => loop {
                let id = new_id('i');
                if !taken.contains(&id) {
                    break id;
                }
            },
        };
        if !taken.insert(id.clone()) {
            bail!("item id {id} is already on the canvas");
        }
        let (w, h) = default_size(&kind);
        let by = match (by, input.by.as_deref()) {
            ("person", Some("agent")) => "agent",
            _ => by,
        };
        let mut item = Item {
            id,
            kind,
            x: f64::NAN,
            y: f64::NAN,
            w: input.w.unwrap_or(w),
            h: input.h.unwrap_or(h),
            text: None,
            caption: None,
            upload_id: None,
            target: None,
            command: None,
            color: None,
            locked: false,
            points: None,
            spec: None,
            by: by.to_string(),
        };
        apply(&mut item, input)?;
        Ok(item)
    }

    pub fn apply_ops(&mut self, ops: Vec<Op>, by: &str) -> Result<Vec<String>> {
        let mut added = Vec::new();
        for op in ops {
            match op {
                Op::Add { items, edges } => {
                    let mut taken: HashSet<String> = self.items.iter().map(|i| i.id.clone()).collect();
                    let filled = items.into_iter().map(|input| self.fill(input, by, &mut taken)).collect::<Result<Vec<_>>>()?;
                    let (placed, loose): (Vec<Item>, Vec<Item>) = filled.into_iter().partition(|i| !i.x.is_nan() && !i.y.is_nan());
                    added.extend(placed.iter().map(|i| i.id.clone()));
                    self.items.extend(placed);
                    let origin = self.bounds().map(|(x0, _, _, y1)| (x0, y1 + GAP * 2.0)).unwrap_or((0.0, 0.0));
                    let mut cursor = origin.0;
                    for mut item in loose {
                        item.x = if item.x.is_nan() { cursor } else { item.x };
                        item.y = if item.y.is_nan() { origin.1 } else { item.y };
                        cursor = item.x + item.w + GAP;
                        added.push(item.id.clone());
                        self.items.push(item);
                    }
                    self.connect(edges)?;
                }
                Op::Update { items } => {
                    for input in items {
                        let id = input.id.as_deref().ok_or_else(|| anyhow!("update needs the id of each item"))?.trim().to_string();
                        let at = self.index(&id).ok_or_else(|| anyhow!("no item {id} on this canvas"))?;
                        if input.kind.as_deref().is_some_and(|k| k != self.items[at].kind) {
                            bail!("an item keeps its kind; remove it and add a new one instead");
                        }
                        apply(&mut self.items[at], input)?;
                    }
                }
                Op::Remove { ids } => {
                    let gone: HashSet<String> = ids.iter().map(|s| s.trim().to_string()).collect();
                    self.items.retain(|i| !gone.contains(&i.id));
                    self.edges.retain(|e| !gone.contains(&e.from) && !gone.contains(&e.to));
                }
                Op::Connect { edges } => self.connect(edges)?,
                Op::Disconnect { ids } => {
                    let gone: HashSet<String> = ids.iter().map(|s| s.trim().to_string()).collect();
                    self.edges.retain(|e| !gone.contains(&e.id));
                }
                Op::Layout { ids, mode, gap, columns, x, y } => self.layout(&ids, &mode, gap.unwrap_or(GAP), columns, x, y)?,
            }
        }
        Ok(added)
    }

    fn connect(&mut self, edges: Vec<EdgeInput>) -> Result<()> {
        let mut taken: HashSet<String> = self.edges.iter().map(|e| e.id.clone()).collect();
        for input in edges {
            let (from, to) = (input.from.trim().to_string(), input.to.trim().to_string());
            if self.index(&from).is_none() || self.index(&to).is_none() {
                bail!("arrows connect items that are on the canvas ({from} to {to})");
            }
            if from == to {
                bail!("an arrow needs two different items");
            }
            let label = text_field(input.label, MAX_LABEL, "an arrow label")?;
            if let Some(existing) = self.edges.iter_mut().find(|e| e.from == from && e.to == to) {
                existing.label = label;
                continue;
            }
            let id = match input.id.as_deref() {
                Some(raw) => clean_id(raw)?,
                None => loop {
                    let id = new_id('e');
                    if !taken.contains(&id) {
                        break id;
                    }
                },
            };
            if !taken.insert(id.clone()) {
                bail!("arrow id {id} is already used");
            }
            self.edges.push(Edge { id, from, to, label });
        }
        Ok(())
    }

    fn layout(&mut self, ids: &[String], mode: &str, gap: f64, columns: Option<usize>, x: Option<f64>, y: Option<f64>) -> Result<()> {
        if !LAYOUTS.contains(&mode) {
            bail!("layout mode is row, grid or flow");
        }
        let mut order = Vec::new();
        for id in ids {
            let at = self.index(id.trim()).ok_or_else(|| anyhow!("no item {} on this canvas", id.trim()))?;
            if !order.contains(&at) {
                order.push(at);
            }
        }
        if order.is_empty() {
            bail!("layout needs the ids of the items to arrange");
        }
        let gap = gap.clamp(0.0, 1000.0);
        let x0 = x.unwrap_or_else(|| order.iter().map(|&i| self.items[i].x).fold(f64::INFINITY, f64::min));
        let y0 = y.unwrap_or_else(|| order.iter().map(|&i| self.items[i].y).fold(f64::INFINITY, f64::min));
        let cols = match mode {
            "grid" => columns.unwrap_or_else(|| (order.len() as f64).sqrt().ceil() as usize).clamp(1, 50),
            _ => order.len(),
        };
        let width = order.iter().map(|&i| self.items[i].w).fold(0.0, f64::max);
        let mut cursor_y = y0;
        for row in order.chunks(cols) {
            let mut cursor_x = x0;
            let height = row.iter().map(|&i| self.items[i].h).fold(0.0, f64::max);
            for &i in row {
                self.items[i].x = cursor_x;
                self.items[i].y = cursor_y;
                cursor_x += if mode == "grid" { width } else { self.items[i].w } + gap;
            }
            cursor_y += height + gap;
        }
        if mode == "flow" {
            let pairs: Vec<EdgeInput> = order
                .windows(2)
                .map(|w| EdgeInput { id: None, from: self.items[w[0]].id.clone(), to: self.items[w[1]].id.clone(), label: None })
                .filter(|e| !self.edges.iter().any(|x| x.from == e.from && x.to == e.to))
                .collect();
            self.connect(pairs)?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.items.len() > MAX_ITEMS {
            bail!("a canvas holds at most {MAX_ITEMS} items; start another canvas and link them");
        }
        if self.edges.len() > MAX_EDGES {
            bail!("a canvas holds at most {MAX_EDGES} arrows");
        }
        let mut ids = HashSet::new();
        for item in &self.items {
            clean_id(&item.id)?;
            if !ids.insert(item.id.as_str()) {
                bail!("item id {} is used twice", item.id);
            }
            if !KINDS.contains(&item.kind.as_str()) {
                bail!("kind is one of {}", KINDS.join(", "));
            }
            for v in [item.x, item.y] {
                if !v.is_finite() || v.abs() > MAX_COORD {
                    bail!("item {} is placed outside the canvas", item.id);
                }
            }
            for v in [item.w, item.h] {
                if !v.is_finite() || !(MIN_SIZE..=MAX_SIZE).contains(&v) {
                    bail!("item sizes are {MIN_SIZE} to {MAX_SIZE} wide and high");
                }
            }
            let need = |ok: bool, what: &str| if ok { Ok(()) } else { Err(anyhow!("{} item {} needs {what}", item.kind, item.id)) };
            match item.kind.as_str() {
                "image" | "video" | "model" => need(item.upload_id.is_some(), "an upload_id (or a file from a task session)")?,
                "note" | "sticky" => need(item.text.is_some(), "text")?,
                "ref" => need(item.target.as_deref().is_some_and(ref_ok), "a target like quest:<id>, report:<id> or msg:<id>")?,
                "run" => need(item.command.is_some(), "a command")?,
                "ink" => need(item.points.as_ref().is_some_and(|p| p.len() >= 2), "at least two points")?,
                "ui" => need(item.spec.is_some(), "a spec (the same json-render spec as show_ui)")?,
                _ => {}
            }
            if item.color.as_deref().is_some_and(|c| !COLORS.contains(&c)) {
                bail!("color is one of {}", COLORS.join(", "));
            }
        }
        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) || !ids.contains(edge.to.as_str()) {
                bail!("arrow {} points at an item that is not on the canvas", edge.id);
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_BYTES {
            bail!("this canvas is too large; start another canvas and link them");
        }
        Ok(())
    }

    pub fn summary(&self) -> String {
        let mut items: Vec<&Item> = self.items.iter().collect();
        items.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = vec![format!("Canvas with {} items and {} arrows.", self.items.len(), self.edges.len())];
        for item in items {
            let caption = item.caption.as_deref().unwrap_or("").trim();
            let line = match item.kind.as_str() {
                "frame" => format!("## {}", if caption.is_empty() { "Frame" } else { caption }),
                "note" => item.text.clone().unwrap_or_default(),
                "sticky" => format!("> {}", item.text.as_deref().unwrap_or("").replace('\n', "\n> ")),
                "image" => format!("![{}](/api/world/uploads/{})", caption.replace(['[', ']'], ""), item.upload_id.unwrap_or_default()),
                "video" | "model" => format!(
                    "[{}](/api/world/uploads/{})",
                    if caption.is_empty() { if item.kind == "video" { "Video" } else { "3D model" } } else { caption },
                    item.upload_id.unwrap_or_default()
                ),
                "ref" => item.target.clone().unwrap_or_default(),
                "run" => format!("```bash\n{}\n```", item.command.as_deref().unwrap_or("")),
                "ui" => format!("```ui\n{}\n```", item.spec.as_ref().map(Value::to_string).unwrap_or_default()),
                _ => continue,
            };
            out.push(line);
            if !caption.is_empty() && matches!(item.kind.as_str(), "note" | "ref" | "run" | "ui") {
                out.push(format!("_{caption}_"));
            }
        }
        out.join("\n\n")
    }

    pub fn targets(&self) -> Vec<String> {
        let mut out: Vec<String> = self.items.iter().filter_map(|i| i.target.clone()).collect();
        out.sort();
        out.dedup();
        out
    }

    fn reading_order(&self) -> Vec<&Item> {
        let mut items: Vec<&Item> = self.items.iter().filter(|i| i.kind != "ink").collect();
        items.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap_or(std::cmp::Ordering::Equal));
        items
    }

    pub fn describe_item(&self, id: &str, resolved: &HashMap<String, String>, cap: usize) -> Option<String> {
        let item = self.items.iter().find(|i| i.id == id)?;
        let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
        let caption = item.caption.as_deref().map(flat).filter(|c| !c.is_empty());
        let body = match item.kind.as_str() {
            "note" | "sticky" => item.text.as_deref().map(flat),
            "ref" => item.target.as_deref().map(|t| resolved.get(t).map(|r| format!("{r} ({t})")).unwrap_or_else(|| t.to_string())),
            "run" => item.command.as_deref().map(|c| format!("runs `{}`", flat(c))),
            "ui" => item.spec.as_ref().map(spec_text),
            "image" | "video" | "model" => item.upload_id.map(|u| format!("/api/world/uploads/{u}")),
            _ => None,
        };
        let text = match (caption, body) {
            (Some(c), Some(b)) if item.kind == "frame" || matches!(item.kind.as_str(), "image" | "video" | "model") => format!("{c}, {b}"),
            (Some(c), Some(b)) => format!("{b} (caption: {c})"),
            (Some(c), None) => c,
            (None, Some(b)) => b,
            (None, None) => String::new(),
        };
        let text: String = text.chars().take(cap).collect();
        let mut line = format!("{} {}", item.kind, item.id);
        if !text.is_empty() {
            line.push_str(": ");
            line.push_str(&text);
        }
        if item.locked {
            line.push_str(" (locked)");
        }
        Some(line)
    }

    pub fn digest(&self, title: &str, report: Uuid, resolved: &HashMap<String, String>, max: usize) -> String {
        let mut out = format!("Canvas \"{title}\" (report:{report}) with {} items and {} arrows.", self.items.len(), self.edges.len());
        let order = self.reading_order();
        for (n, item) in order.iter().enumerate() {
            let line = self.describe_item(&item.id, resolved, 600).unwrap_or_default();
            let line = if item.kind == "frame" { format!("\n## {}", line) } else { format!("\n- {line}") };
            if out.chars().count() + line.chars().count() > max.saturating_sub(60) {
                out.push_str(&format!("\n... and {} more items", order.len() - n));
                return out;
            }
            out.push_str(&line);
        }
        for e in &self.edges {
            let line = format!("\n- arrow {} -> {}{}", e.from, e.to, e.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default());
            if out.chars().count() + line.chars().count() > max {
                break;
            }
            out.push_str(&line);
        }
        out
    }

    pub fn outline(&self) -> String {
        let mut out = Vec::new();
        for item in &self.items {
            let mut line = format!("- {} {} at ({:.0}, {:.0}) size {:.0}x{:.0}", item.kind, item.id, item.x, item.y, item.w, item.h);
            if let Some(u) = item.upload_id {
                line.push_str(&format!(" upload {u}"));
            }
            if let Some(t) = &item.target {
                line.push_str(&format!(" -> {t}"));
            }
            if let Some(c) = &item.caption {
                line.push_str(&format!(" caption \"{}\"", c.chars().take(120).collect::<String>()));
            }
            if let Some(t) = item.text.clone().or(item.command.clone()).or(item.spec.as_ref().map(spec_text)) {
                line.push_str(&format!(": {}", t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>()));
            }
            if item.locked {
                line.push_str(" (locked)");
            }
            if item.by == "person" {
                line.push_str(" (added by your person)");
            }
            out.push(line);
        }
        for e in &self.edges {
            out.push(format!("- arrow {} {} -> {}{}", e.id, e.from, e.to, e.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default()));
        }
        out.join("\n")
    }
}

pub fn spec_text(spec: &Value) -> String {
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(s) if !s.trim().is_empty() => out.push(s.trim().to_string()),
            Value::Array(list) => list.iter().for_each(|v| walk(v, out)),
            Value::Object(map) => map.iter().filter(|(k, _)| !matches!(k.as_str(), "type" | "children" | "on" | "action" | "kind" | "variant" | "tone")).for_each(|(_, v)| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(elements) = spec.get("elements").and_then(Value::as_object) {
        for el in elements.values() {
            let kind = el.get("type").and_then(Value::as_str).unwrap_or("");
            let mut words = Vec::new();
            if let Some(props) = el.get("props") {
                walk(props, &mut words);
            }
            if !words.is_empty() {
                out.push(format!("{kind} {}", words.join(" ")));
            }
        }
    }
    let text = out.join("; ");
    if text.is_empty() { "ui card".into() } else { text.chars().take(1200).collect() }
}

pub fn quote_source(source: &str) -> Option<(&str, Option<&str>)> {
    let rest = source.strip_prefix("report:")?;
    let (id, item) = match rest.split_once('#') {
        Some((id, item)) => (id, Some(item)),
        None => (rest, None),
    };
    let id_ok = (6..=36).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let item_ok = item.is_none_or(|i| clean_id(i).is_ok_and(|c| c == i));
    (id_ok && item_ok).then_some((id, item))
}

fn apply(item: &mut Item, input: ItemInput) -> Result<()> {
    if let Some(v) = input.x {
        item.x = v;
    }
    if let Some(v) = input.y {
        item.y = v;
    }
    if let Some(v) = input.w {
        item.w = v;
    }
    if let Some(v) = input.h {
        item.h = v;
    }
    if input.text.is_some() {
        item.text = text_field(input.text, MAX_TEXT, "text")?;
    }
    if input.caption.is_some() {
        item.caption = text_field(input.caption, MAX_LABEL * 2, "a caption")?;
    }
    if input.upload_id.is_some() {
        item.upload_id = input.upload_id;
    }
    if let Some(t) = input.target {
        item.target = Some(t.trim().to_string()).filter(|t| !t.is_empty());
    }
    if input.command.is_some() {
        item.command = text_field(input.command, MAX_COMMAND, "a command")?.map(|c| c.trim().to_string());
    }
    if let Some(c) = input.color {
        item.color = Some(c.trim().to_lowercase()).filter(|c| !c.is_empty());
    }
    if let Some(l) = input.locked {
        item.locked = l;
    }
    if let Some(spec) = input.spec {
        item.spec = Some(crate::ui_cards::normalize(&spec).map_err(|e| anyhow!("ui item: {e}"))?);
    }
    if let Some(points) = input.points {
        if points.len() > MAX_INK_POINTS {
            bail!("a pen stroke has at most {MAX_INK_POINTS} points");
        }
        if points.iter().any(|p| !p[0].is_finite() || !p[1].is_finite()) {
            bail!("pen points are numbers");
        }
        let (x0, y0) = points.iter().fold((f64::INFINITY, f64::INFINITY), |(x, y), p| (x.min(p[0]), y.min(p[1])));
        let (x1, y1) = points.iter().fold((f64::NEG_INFINITY, f64::NEG_INFINITY), |(x, y), p| (x.max(p[0]), y.max(p[1])));
        let pad = 8.0;
        item.x = x0 - pad;
        item.y = y0 - pad;
        item.w = (x1 - x0 + pad * 2.0).clamp(MIN_SIZE, MAX_SIZE);
        item.h = (y1 - y0 + pad * 2.0).clamp(MIN_SIZE, MAX_SIZE);
        item.points = Some(points.iter().map(|p| [((p[0] - item.x) * 10.0).round() / 10.0, ((p[1] - item.y) * 10.0).round() / 10.0]).collect());
    }
    Ok(())
}

pub fn parse_ops(value: &Value) -> Result<Vec<Op>> {
    let list = match value {
        Value::Array(_) => value.clone(),
        Value::Object(_) => Value::Array(vec![value.clone()]),
        _ => bail!("ops is a list of changes"),
    };
    serde_json::from_value(list).map_err(|e| anyhow!("could not read the canvas change: {e}"))
}

fn mime_fits(kind: &str, mime: &str) -> bool {
    match kind {
        "image" => crate::media::IMAGE_TYPES.contains(&mime),
        "video" => crate::media::VIDEO_TYPES.contains(&mime),
        "model" => mime == MODEL_MIME,
        _ => true,
    }
}

impl WorldStore {
    pub(crate) async fn check_canvas_uploads(&self, owner: Uuid, org: Option<Uuid>, canvas: &Canvas, before: &Canvas) -> Result<()> {
        let known: HashSet<Uuid> = before.upload_ids().into_iter().collect();
        let fresh: Vec<Uuid> = canvas.upload_ids().into_iter().filter(|u| !known.contains(u)).collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let found: HashMap<Uuid, String> = self.own_uploads(&fresh, owner, org).await?.into_iter().map(|u| (u.id, u.mime)).collect();
        for item in &canvas.items {
            let Some(id) = item.upload_id.filter(|u| fresh.contains(u)) else { continue };
            let Some(mime) = found.get(&id) else {
                bail!("upload {id} is not one of your person's uploads in this organization");
            };
            if !mime_fits(&item.kind, mime) {
                bail!("upload {id} is {mime}, which does not fit a {} item", item.kind);
            }
        }
        Ok(())
    }

    pub async fn create_canvas(&self, new: crate::reports::NewReport, canvas: Canvas) -> Result<Report> {
        self.create_canvas_with(new, canvas, None).await
    }

    pub(crate) async fn create_canvas_with(&self, mut new: crate::reports::NewReport, canvas: Canvas, practice: Option<&Value>) -> Result<Report> {
        canvas.validate()?;
        new.body = canvas.summary();
        self.insert_report(new, Some(&canvas), practice).await
    }

    pub async fn change_canvas(&self, id: Uuid, by: ReportActor, ops: Vec<Op>) -> Result<Option<(Report, Vec<String>)>> {
        let reshapes = ops.iter().any(Op::reshapes);
        let mut tx = self.pool().begin().await?;
        let Some(current) = sqlx::query_as::<_, Report>("SELECT * FROM world_reports WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await? else {
            return Ok(None);
        };
        if current.kind != "canvas" {
            bail!("\"{}\" is a written report, not a canvas", current.title);
        }
        let before = Canvas::parse(current.canvas.as_ref())?;
        let mut canvas = before.clone();
        let added = canvas.apply_ops(ops, if matches!(by, ReportActor::User(_)) { "person" } else { "agent" })?;
        canvas.validate()?;
        if canvas == before {
            tx.rollback().await?;
            return Ok(Some((current, added)));
        }
        self.check_canvas_uploads(current.owner_user_id, current.org_id, &canvas, &before).await?;
        let body = canvas.summary();
        let links = self.resolve_report_links(current.owner_user_id, current.org_id, &body, Some(id)).await?;
        let value = canvas.to_value();
        let (revision, merged) = self
            .record_revision(&mut tx, &current, by, (&current.title, &body, &current.folder, &current.tags), Some(&value))
            .await?;
        let report = sqlx::query_as::<_, Report>(
            "UPDATE world_reports SET canvas = $2, body = $3, links = $4, revision = $5, updated_at = NOW() WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(&value)
        .bind(&body)
        .bind(&links)
        .bind(revision)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        self.report_event("report.updated", &report, by, merged || !reshapes).await?;
        Ok(Some((report, added)))
    }

    pub async fn describe_refs(&self, viewer: Uuid, org: Uuid, targets: &[String]) -> Result<HashMap<String, String>> {
        let flat = |t: &str, n: usize| t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect::<String>();
        let mut wanted: HashMap<&str, RefIds> = HashMap::new();
        let mut channel_ids: Vec<i64> = Vec::new();
        let mut slugs: Vec<String> = Vec::new();
        for target in targets.iter().take(80) {
            let Some((kind, raw)) = target.split_once(':') else { continue };
            match (kind, raw.strip_prefix("ch").and_then(|n| n.parse::<i64>().ok())) {
                ("msg", Some(id)) => channel_ids.push(id),
                ("quest" | "report" | "todo" | "msg" | "practice", None) => wanted.entry(kind).or_default().push(raw),
                ("agent", _) => {
                    slugs.push(raw.to_lowercase());
                    wanted.entry(kind).or_default().push(raw);
                }
                _ => {}
            }
        }
        let mut rows: HashMap<&str, Vec<RefRow>> = HashMap::new();
        for (kind, ids) in &wanted {
            let (sql, alias) = match *kind {
                "quest" => (
                    "SELECT t.id, t.title AS a, t.status AS b, '' AS c FROM agent_tasks t LEFT JOIN agent_profiles x ON x.id = t.assigned_agent_id
                     WHERE t.org_id = $1 AND (t.user_id = $2 OR x.owner_user_id = $2 OR x.visibility = 'public') AND {match}
                     ORDER BY t.created_at DESC LIMIT 200",
                    "t",
                ),
                "report" => (
                    "SELECT r.id, r.title AS a, r.kind AS b, left(r.body, 400) AS c FROM world_reports r
                     WHERE r.org_id = $1 AND (r.owner_user_id = $2 OR r.visibility = 'public') AND {match}
                     ORDER BY r.updated_at DESC LIMIT 200",
                    "r",
                ),
                "practice" => (
                    "SELECT p.id, r.title AS a, p.status AS b, p.n::text || ' ' || COALESCE(p.\"check\"->>'fix', '') AS c
                     FROM world_practice_attempts p JOIN world_reports r ON r.id = p.report_id
                     WHERE r.org_id = $1 AND (r.owner_user_id = $2 OR r.visibility = 'public') AND {match}
                     ORDER BY p.created_at DESC LIMIT 200",
                    "p",
                ),
                "todo" => (
                    "SELECT d.id, d.text AS a, d.status AS b, '' AS c FROM world_todos d
                     WHERE d.org_id = $1 AND d.owner_user_id = $2 AND {match}
                     ORDER BY d.updated_at DESC LIMIT 200",
                    "d",
                ),
                "msg" => (
                    "SELECT m.id, COALESCE(au.name, u.name, 'someone') AS a, t.name AS b, m.body AS c FROM agent_messages m
                     JOIN agent_profiles t ON t.id = m.agent_id
                     LEFT JOIN agent_profiles au ON au.id = m.author_agent_id
                     LEFT JOIN users u ON u.id = m.author_user_id
                     WHERE t.org_id = $1 AND (t.owner_user_id = $2 OR t.visibility = 'public') AND {thread} AND {match}
                     ORDER BY m.created_at DESC LIMIT 200",
                    "m",
                ),
                _ => (
                    "SELECT g.id, g.name AS a, COALESCE(g.expertise, '') AS b, g.slug AS c FROM agent_profiles g
                     WHERE g.org_id = $1 AND (g.owner_user_id = $2 OR g.visibility = 'public') AND (lower(g.slug) = ANY($4) OR {match})
                     LIMIT 200",
                    "g",
                ),
            };
            let sql = sql.replace("{thread}", &crate::guests::thread_filter("m.agent_id", "$2"));
            let mut found = Vec::new();
            if !ids.full.is_empty() {
                found.extend(
                    sqlx::query_as::<_, RefRow>(&sql.replace("{match}", &format!("{alias}.id = ANY($3)")))
                        .bind(org)
                        .bind(viewer)
                        .bind(&ids.full)
                        .bind(&slugs)
                        .fetch_all(self.pool())
                        .await?,
                );
            }
            if !ids.prefixes.is_empty() || (*kind == "agent" && !slugs.is_empty()) {
                let patterns: Vec<String> = ids.prefixes.iter().map(|p| format!("{p}%")).collect();
                found.extend(
                    sqlx::query_as::<_, RefRow>(&sql.replace("{match}", &format!("replace({alias}.id::text, '-', '') LIKE ANY($3)")))
                        .bind(org)
                        .bind(viewer)
                        .bind(&patterns)
                        .bind(&slugs)
                        .fetch_all(self.pool())
                        .await?,
                );
            }
            rows.insert(kind, found);
        }
        let channel_rows: Vec<(i64, String, String, String)> = if channel_ids.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "SELECT m.id, COALESCE(au.name, u.name, 'someone'), c.name, m.body FROM channel_messages m
                 JOIN channels c ON c.id = m.channel_id
                 LEFT JOIN agent_profiles au ON au.id = m.author_agent_id
                 LEFT JOIN users u ON u.id = m.author_user_id
                 WHERE m.id = ANY($2) AND c.org_id = $1
                   AND (c.kind = 'channel' OR EXISTS (SELECT 1 FROM channel_members cm WHERE cm.channel_id = c.id AND cm.member_kind = 'user' AND cm.member_id = $3))",
            )
            .bind(org)
            .bind(&channel_ids)
            .bind(viewer)
            .fetch_all(self.pool())
            .await?
        };
        let mut out = HashMap::new();
        for target in targets.iter().take(80) {
            let Some((kind, raw)) = target.split_once(':') else { continue };
            let line = if let Some(id) = raw.strip_prefix("ch").and_then(|n| n.parse::<i64>().ok()).filter(|_| kind == "msg") {
                channel_rows
                    .iter()
                    .find(|r| r.0 == id)
                    .map(|(_, author, channel, body)| format!("Message from {author} in #{channel}: {}", flat(body, 400)))
            } else {
                rows.get(kind).and_then(|found| pick_ref(found, kind, raw)).map(|row| match kind {
                    "quest" => format!("Quest \"{}\" ({})", flat(&row.a, 160), row.b.replace('_', " ")),
                    "report" => format!("{} \"{}\": {}", if row.b == "canvas" { "Canvas" } else { "Report" }, flat(&row.a, 160), flat(&row.c, 200)),
                    "todo" => format!("To-do \"{}\" ({})", flat(&row.a, 200), row.b),
                    "practice" => {
                        let (n, fix) = row.c.split_once(' ').unwrap_or((row.c.as_str(), ""));
                        let fix = if fix.is_empty() { String::new() } else { format!(", fix: {}", flat(fix, 160)) };
                        format!("Practice attempt {n} on \"{}\" ({}){fix}", flat(&row.a, 160), row.b)
                    }
                    "msg" => format!("Message from {} in {}'s chat: {}", row.a, row.b, flat(&row.c, 400)),
                    _ if row.b.is_empty() => format!("Agent {}", row.a),
                    _ => format!("Agent {}, {}", row.a, flat(&row.b, 120)),
                })
            };
            if let Some(line) = line {
                out.insert(target.clone(), line);
            }
        }
        Ok(out)
    }

    pub async fn canvas_digest(&self, viewer: Uuid, org: Uuid, report: &Report, max: usize) -> Result<(Canvas, HashMap<String, String>, String)> {
        let canvas = Canvas::parse(report.canvas.as_ref())?;
        let resolved = self.describe_refs(viewer, org, &canvas.targets()).await?;
        let digest = canvas.digest(&report.title, report.id, &resolved, max);
        Ok((canvas, resolved, digest))
    }

    pub async fn expand_canvas_quote(&self, viewer: Uuid, org: Uuid, quote: &mut Value, cards: QuoteCards) -> Result<()> {
        let Some(source) = quote.get("source").and_then(Value::as_str).map(String::from) else { return Ok(()) };
        let Some((id, item)) = quote_source(&source) else { return Ok(()) };
        let Some(report) = self.find_report(viewer, org, id).await? else { return Ok(()) };
        if report.kind != "canvas" {
            return Ok(());
        }
        let (canvas, resolved, whole) = match cards {
            QuoteCards::Resolved => self.canvas_digest(viewer, org, &report, crate::quotes::CONTEXT_CHARS).await?,
            QuoteCards::RefsOnly => {
                let canvas = Canvas::parse(report.canvas.as_ref())?;
                let whole = canvas.digest(&report.title, report.id, &HashMap::new(), crate::quotes::CONTEXT_CHARS);
                (canvas, HashMap::new(), whole)
            }
        };
        let context = match item.and_then(|i| canvas.describe_item(i, &resolved, 1500)) {
            Some(line) => format!("The quoted item: {line}\n\n{whole}"),
            None => whole,
        };
        quote["context"] = json!(context.chars().take(crate::quotes::CONTEXT_CHARS).collect::<String>());
        Ok(())
    }

    pub async fn search_canvas_items(&self, viewer: Uuid, org: Uuid, query: &str, limit: i64) -> Result<Vec<CanvasHit>> {
        let pattern = crate::search::like_pattern(query);
        Ok(sqlx::query_as::<_, CanvasHit>(&format!(
            "SELECT r.id, r.title, r.folder, r.updated_at, {items} AS items FROM world_reports r
             WHERE r.org_id = $1 AND r.kind = 'canvas' AND (r.owner_user_id = $2 OR r.visibility = 'public')
               AND (r.title ILIKE $3 OR r.body ILIKE $3 OR {items} <> '[]'::jsonb)
             ORDER BY (r.title ILIKE $3) DESC, r.updated_at DESC LIMIT $4",
            items = item_hits_sql("$3")
        ))
        .bind(org)
        .bind(viewer)
        .bind(&pattern)
        .bind(limit.clamp(1, 50))
        .fetch_all(self.pool())
        .await?)
    }

    pub async fn remove_canvas_mark(&self, report: &Report, mark: Uuid, by: ReportActor) -> Result<bool> {
        let gone = sqlx::query_scalar::<_, Uuid>("DELETE FROM world_canvas_marks WHERE id = $1 AND report_id = $2 RETURNING id")
            .bind(mark)
            .bind(report.id)
            .fetch_optional(self.pool())
            .await?
            .is_some();
        if gone {
            let mut event = NewEvent::new("canvas.mark_removed").task(report.task_id).payload(json!({
                "report_id": report.id,
                "org_id": report.org_id,
                "owner_user_id": report.owner_user_id,
                "mark_id": mark,
                "quiet": true,
            }));
            event = match by {
                ReportActor::Agent(a) => event.agent(a),
                ReportActor::User(u) => event.actor(Some(u)),
            };
            self.emit(event, None).await?;
        }
        Ok(gone)
    }

    pub async fn canvas_marks(&self, report: Uuid) -> Result<Vec<Mark>> {
        Ok(sqlx::query_as::<_, Mark>("SELECT * FROM world_canvas_marks WHERE report_id = $1 ORDER BY created_at").bind(report).fetch_all(self.pool()).await?)
    }

    pub async fn add_canvas_mark(&self, report: &Report, author: Uuid, mark: NewMark) -> Result<Mark> {
        if report.kind != "canvas" {
            bail!("marks go on canvases");
        }
        let body = text_field(Some(mark.body), MAX_MARK, "a mark")?.ok_or_else(|| anyhow!("say what you would like changed"))?;
        let kind = match mark.kind.as_deref().unwrap_or("pin") {
            k @ ("pin" | "sticky") => k.to_string(),
            _ => bail!("a mark is a pin or a sticky"),
        };
        if !mark.x.is_finite() || !mark.y.is_finite() || mark.x.abs() > MAX_COORD || mark.y.abs() > MAX_COORD {
            bail!("the mark is outside the canvas");
        }
        let canvas = Canvas::parse(report.canvas.as_ref())?;
        let item = match mark.item_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) if canvas.index(id).is_some() => Some(id.to_string()),
            Some(id) => bail!("no item {id} on this canvas"),
            None => None,
        };
        let near = if kind == "sticky" { canvas.near_item(item.as_deref(), (mark.x, mark.y)) } else { None };
        let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_canvas_marks WHERE report_id = $1 AND status = 'open'").bind(report.id).fetch_one(self.pool()).await?;
        if open >= MAX_OPEN_MARKS {
            bail!("this canvas has {open} open marks; resolve some first");
        }
        let saved = sqlx::query_as::<_, Mark>(
            "INSERT INTO world_canvas_marks (report_id, item_id, x, y, kind, body, author_user_id) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(report.id)
        .bind(&item)
        .bind(mark.x)
        .bind(mark.y)
        .bind(&kind)
        .bind(&body)
        .bind(author)
        .fetch_one(self.pool())
        .await?;
        let author_name: Option<String> = sqlx::query_scalar("SELECT name FROM users WHERE id = $1").bind(author).fetch_optional(self.pool()).await?.flatten();
        let mut event = NewEvent::new("canvas.mark").actor(Some(author)).task(report.task_id).payload(json!({
            "report_id": report.id,
            "org_id": report.org_id,
            "owner_user_id": report.owner_user_id,
            "title": report.title,
            "mark_id": saved.id,
            "kind": kind,
            "item_id": item,
            "item": item.as_deref().and_then(|i| canvas.item_line(i)),
            "sticky_id": canvas.sticky_of(item.as_deref()),
            "near_item_id": near,
            "near_item": near.as_deref().and_then(|i| canvas.item_line(i)),
            "text": body,
            "author": author_name.unwrap_or_else(|| "Your person".into()),
        }));
        let wake = report.author_agent_id.filter(|_| author == report.owner_user_id);
        if let Some(agent) = report.author_agent_id {
            event = event.agent(agent);
        }
        self.emit(event, wake.map(|a| (a, "canvas.mark"))).await?;
        Ok(saved)
    }

    pub async fn resolve_canvas_mark(&self, report: &Report, mark: Uuid, by: ReportActor, reply: Option<String>, reopen: bool) -> Result<Option<Mark>> {
        let reply = text_field(reply, MAX_MARK, "a reply")?;
        let saved = sqlx::query_as::<_, Mark>(
            "UPDATE world_canvas_marks SET status = CASE WHEN $3 THEN 'open' ELSE 'resolved' END,
               reply = COALESCE($4, reply), resolved_by_agent_id = CASE WHEN $3 THEN NULL ELSE $5 END,
               resolved_by_user_id = CASE WHEN $3 THEN NULL ELSE $6 END, resolved_at = CASE WHEN $3 THEN NULL ELSE NOW() END
             WHERE id = $1 AND report_id = $2 RETURNING *",
        )
        .bind(mark)
        .bind(report.id)
        .bind(reopen)
        .bind(&reply)
        .bind(match by {
            ReportActor::Agent(a) => Some(a),
            ReportActor::User(_) => None,
        })
        .bind(match by {
            ReportActor::User(u) => Some(u),
            ReportActor::Agent(_) => None,
        })
        .fetch_optional(self.pool())
        .await?;
        if let Some(m) = &saved {
            let mut event = NewEvent::new("canvas.mark_resolved").task(report.task_id).payload(json!({
                "report_id": report.id,
                "org_id": report.org_id,
                "owner_user_id": report.owner_user_id,
                "mark_id": m.id,
                "status": m.status,
                "quiet": true,
            }));
            event = match by {
                ReportActor::Agent(a) => event.agent(a),
                ReportActor::User(u) => event.actor(Some(u)),
            };
            self.emit(event, None).await?;
        }
        Ok(saved)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QuoteCards {
    Resolved,
    RefsOnly,
}

#[derive(Default)]
struct RefIds {
    full: Vec<Uuid>,
    prefixes: Vec<String>,
}

impl RefIds {
    fn push(&mut self, raw: &str) {
        let hex: String = raw.chars().filter(|c| *c != '-').collect::<String>().to_lowercase();
        if hex.len() == 32 {
            if let Ok(id) = Uuid::parse_str(&hex) {
                self.full.push(id);
                return;
            }
        }
        if hex.len() >= 6 && hex.len() < 32 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            self.prefixes.push(hex);
        }
    }
}

#[derive(FromRow)]
struct RefRow {
    id: Uuid,
    a: String,
    b: String,
    c: String,
}

fn pick_ref<'a>(rows: &'a [RefRow], kind: &str, raw: &str) -> Option<&'a RefRow> {
    let hex: String = raw.chars().filter(|c| *c != '-').collect::<String>().to_lowercase();
    if kind == "agent" {
        if let Some(row) = rows.iter().find(|r| r.c.eq_ignore_ascii_case(raw)) {
            return Some(row);
        }
    }
    if hex.len() < 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    rows.iter().find(|r| r.id.simple().to_string().starts_with(&hex))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::NewReport;
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn canvases_revise_mark_and_wake_their_agent() -> Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Canvas owner') RETURNING id")
            .bind(format!("{tag}@canvas.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Painter', 'claude', 'opus', $2) RETURNING id")
            .bind(format!("painter-{}", &tag[..8]))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let by = ReportActor::Agent(agent);
        let mut start = Canvas::default();
        start.apply_ops(parse_ops(&json!({ "op": "add", "items": [{ "id": "a", "kind": "note", "text": "Option A" }, { "id": "b", "kind": "note", "text": "Option B" }] }))?, "agent")?;
        let new = NewReport { owner_user_id: owner, by, title: "Jinn looks".into(), body: String::new(), folder: None, tags: vec![], visibility: None, pinned: false, task_id: None };
        let report = store.create_canvas(new, start).await?;
        assert_eq!((report.kind.as_str(), report.revision), ("canvas", 1));
        assert!(report.body.contains("Option A"));
        let stranger = Uuid::new_v4();
        let refused = store.change_canvas(report.id, by, parse_ops(&json!({ "op": "add", "items": [{ "kind": "image", "upload_id": stranger }] }))?).await;
        assert!(refused.is_err());
        let (moved, _) = store.change_canvas(report.id, by, parse_ops(&json!([{ "op": "connect", "edges": [{ "from": "a", "to": "b", "label": "then" }] }, { "op": "layout", "ids": ["b", "a"], "mode": "row" }]))?).await?.unwrap();
        assert_eq!(moved.revision, 2);
        let (again, _) = store.change_canvas(report.id, by, parse_ops(&json!({ "op": "update", "items": [{ "id": "a", "locked": true }] }))?).await?.unwrap();
        assert_eq!(again.revision, 2);
        let (by_person, _) = store.change_canvas(report.id, ReportActor::User(owner), parse_ops(&json!({ "op": "add", "items": [{ "id": "s", "kind": "sticky", "text": "hmm" }] }))?).await?.unwrap();
        assert_eq!(by_person.revision, 3);
        let old = store.report_revision(report.id, 1).await?.unwrap();
        assert_eq!(Canvas::parse(old.canvas.as_ref())?.items.len(), 2);
        let restored = store.restore_report_revision(report.id, 1, ReportActor::User(owner)).await?.unwrap();
        assert_eq!(Canvas::parse(restored.canvas.as_ref())?.items.len(), 2);
        assert!(store.update_report(report.id, by, &crate::reports::ReportPatch { body: Some("x".into()), ..Default::default() }).await.is_err());

        let mark = store.add_canvas_mark(&restored, owner, NewMark { item_id: Some("b".into()), body: "not sure about B".into(), ..Default::default() }).await?;
        assert!(store.add_canvas_mark(&restored, owner, NewMark { item_id: Some("zz".into()), body: "x".into(), ..Default::default() }).await.is_err());
        let woke: (String, Value) = sqlx::query_as("SELECT kind, payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(agent).fetch_one(&pool).await?;
        assert_eq!(woke.0, "canvas.mark");
        assert_eq!(woke.1["from_owner"], json!(true));
        assert_eq!(woke.1["item_id"], json!("b"));
        let brief = store.list_reports(owner, restored.org_id.unwrap_or(crate::orgs::DEFAULT_ORG), &crate::reports::ReportFilter::default()).await?;
        assert_eq!(brief.iter().find(|b| b.id == report.id).map(|b| (b.kind.as_str(), b.open_marks)), Some(("canvas", 1)));
        let done = store.resolve_canvas_mark(&restored, mark.id, by, Some("Made B heavier".into()), false).await?.unwrap();
        assert_eq!((done.status.as_str(), done.resolved_by_agent_id), ("resolved", Some(agent)));
        let b = Canvas::parse(restored.canvas.as_ref())?.items.into_iter().find(|i| i.id == "b").unwrap();
        let stuck = parse_ops(&json!({ "op": "add", "items": [{ "id": "s2", "kind": "sticky", "text": "heavier?", "x": b.x + 20.0, "y": b.y + 20.0 }] }))?;
        let (current, _) = store.change_canvas(report.id, ReportActor::User(owner), stuck).await?.unwrap();
        store.add_canvas_mark(&current, owner, NewMark { item_id: Some("s2".into()), kind: Some("sticky".into()), body: "heavier?".into(), ..Default::default() }).await?;
        let woke: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(agent).fetch_one(&pool).await?;
        assert_eq!((woke["item_id"].clone(), woke["near_item_id"].clone()), (json!("s2"), json!("b")));
        assert_eq!(woke["near_item"], json!("note b: Option B"));
        assert_eq!(woke["sticky_id"], json!("s2"));
        store.add_canvas_mark(&current, owner, NewMark { item_id: Some("b".into()), kind: Some("sticky".into()), body: "on b itself".into(), ..Default::default() }).await?;
        let hosted: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(agent).fetch_one(&pool).await?;
        assert_eq!((hosted["near_item_id"].clone(), hosted["sticky_id"].clone()), (json!("b"), Value::Null));
        store.add_canvas_mark(&current, owner, NewMark { x: 9000.0, y: 9000.0, body: "far pin".into(), ..Default::default() }).await?;
        let pin: Value = sqlx::query_scalar("SELECT payload FROM agent_inbox WHERE agent_id = $1 ORDER BY id DESC LIMIT 1").bind(agent).fetch_one(&pool).await?;
        assert_eq!(pin["near_item_id"], Value::Null);

        sqlx::query("DELETE FROM world_events WHERE agent_id = $1 OR payload->>'owner_user_id' = $2").bind(agent).bind(owner.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_inbox WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = $1").bind(owner).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn stickies_find_the_topmost_item_under_them_frames_last() {
        let raw = json!({ "items": [
            { "id": "f", "kind": "frame", "x": 0, "y": 0, "w": 800, "h": 600, "caption": "Round 1" },
            { "id": "a", "kind": "note", "text": "A", "x": 40, "y": 40, "w": 200, "h": 200 },
            { "id": "b", "kind": "note", "text": "B", "x": 100, "y": 100, "w": 200, "h": 200 },
            { "id": "s", "kind": "sticky", "text": "hmm", "x": 150, "y": 150, "w": 100, "h": 100 },
            { "id": "t", "kind": "sticky", "text": "and here", "x": 600, "y": 500, "w": 100, "h": 50 },
            { "id": "u", "kind": "sticky", "text": "nowhere", "x": 2000, "y": 2000, "w": 100, "h": 50 },
            { "id": "v", "kind": "sticky", "text": "on A, spilling onto B", "x": 50, "y": 50, "w": 200, "h": 200 }
        ] });
        let c = Canvas::from_raw(&raw, "person").unwrap();
        assert_eq!(c.near_item(Some("s"), (0.0, 0.0)).as_deref(), Some("b"));
        assert_eq!(c.near_item(Some("t"), (0.0, 0.0)).as_deref(), Some("f"));
        assert_eq!(c.near_item(Some("u"), (0.0, 0.0)), None);
        assert_eq!(c.near_item(Some("v"), (0.0, 0.0)).as_deref(), Some("a"));
        assert_eq!(c.near_item(None, (50.0, 50.0)).as_deref(), Some("a"));
        assert_eq!(c.near_item(None, (150.0, 150.0)).as_deref(), Some("b"));
        assert_eq!(c.near_item(Some("a"), (0.0, 0.0)).as_deref(), Some("a"));
        assert_eq!(c.near_item(Some("gone"), (50.0, 50.0)), None);
        assert_eq!(c.sticky_of(Some("s")), Some("s"));
        assert_eq!(c.sticky_of(Some("a")), None);
    }

    #[test]
    fn raw_canvas_json_is_normalized_like_tool_input() {
        let raw = json!({
            "items": [
                { "kind": "note", "text": "  no id or place  ", "by": "agent" },
                { "id": "k", "kind": "ink", "x": 100, "y": 200, "w": 50, "h": 50, "points": [[8, 8], [40, 30]] },
                { "id": "n", "kind": "note", "text": "placed", "x": 10, "y": 10 }
            ],
            "edges": [{ "from": "n", "to": "k", "label": "then" }]
        });
        let c = Canvas::from_raw(&raw, "person").unwrap();
        assert_eq!(c.items.len(), 3);
        assert!(c.items.iter().all(|i| i.by == "person" && !i.id.is_empty() && i.x.is_finite() && i.y.is_finite()));
        let ink = c.items.iter().find(|i| i.id == "k").unwrap();
        assert_eq!((ink.x, ink.y), (100.0, 200.0));
        assert_eq!(ink.points.as_deref(), Some(&[[8.0, 8.0], [40.0, 30.0]][..]));
        assert!(c.edges[0].id.len() > 1 && c.edges[0].label.as_deref() == Some("then"));
        let ids: Vec<&str> = c.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(&ids[1..], ["k", "n"]);
        assert_eq!(c.items[0].text.as_deref().map(str::trim), Some("no id or place"));

        let refusals = [
            json!({ "items": [{ "id": "x", "kind": "blob" }] }),
            json!({ "items": [{ "id": "x", "kind": "note", "text": "a" }, { "id": "x", "kind": "note", "text": "b" }] }),
            json!({ "items": [{ "id": "x", "kind": "note", "text": "a".repeat(MAX_TEXT + 1) }] }),
            json!({ "items": [{ "id": "x", "kind": "note", "text": "a", "color": "neon" }] }),
            json!({ "items": [{ "id": "x", "kind": "note", "text": "a" }], "edges": [{ "from": "x", "to": "y" }] }),
            json!({ "items": [{ "id": "x", "kind": "note" }] }),
            json!({ "items": "nope" }),
        ];
        for bad in refusals {
            assert!(Canvas::from_raw(&bad, "person").is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn canvas_refs_quotes_search_and_marks() -> Result<()> {
        let Ok(url) = std::env::var("REPORTS_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let pool = PgPoolOptions::new().max_connections(4).connect(&url).await?;
        let store = WorldStore::new(pool.clone());
        let tag = Uuid::new_v4().simple().to_string();
        let owner: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Board owner') RETURNING id")
            .bind(format!("{tag}@board.test"))
            .fetch_one(&pool)
            .await?;
        let stranger: Uuid = sqlx::query_scalar("INSERT INTO users (email, name) VALUES ($1, 'Stranger') RETURNING id")
            .bind(format!("{tag}-x@board.test"))
            .fetch_one(&pool)
            .await?;
        let agent: Uuid = sqlx::query_scalar("INSERT INTO agent_profiles (slug, name, cli, model, owner_user_id) VALUES ($1, 'Boarder', 'claude', 'opus', $2) RETURNING id")
            .bind(format!("boarder-{}", &tag[..8]))
            .bind(owner)
            .fetch_one(&pool)
            .await?;
        let org: Uuid = sqlx::query_scalar("SELECT org_id FROM agent_profiles WHERE id = $1").bind(agent).fetch_one(&pool).await?;
        let quest: Uuid = sqlx::query_scalar("INSERT INTO agent_tasks (user_id, title, repo, org_id, assigned_agent_id) VALUES ($1, 'Ship the drawbridge', 'acme/site', $2, $3) RETURNING id")
            .bind(owner)
            .bind(org)
            .bind(agent)
            .fetch_one(&pool)
            .await?;
        let todo: Uuid = sqlx::query_scalar("INSERT INTO world_todos (owner_user_id, text, org_id) VALUES ($1, 'Oil the hinges', $2) RETURNING id")
            .bind(owner)
            .bind(org)
            .fetch_one(&pool)
            .await?;
        let by = ReportActor::Agent(agent);
        let mut start = Canvas::default();
        let word = format!("moat{}", &tag[..6]);
        start.apply_ops(
            parse_ops(&json!({ "op": "add", "items": [
                { "id": "q", "kind": "ref", "target": format!("quest:{quest}") },
                { "id": "t", "kind": "ref", "target": format!("todo:{todo}") },
                { "id": "n", "kind": "note", "text": format!("Dig the {word} first") },
                { "id": "u", "kind": "ui", "spec": { "root": "m", "elements": { "m": { "type": "Metric", "props": { "label": "Bridges", "value": "3" } } } } }
            ] }))?,
            "agent",
        )?;
        let new = NewReport { owner_user_id: owner, by, title: "Castle plan".into(), body: String::new(), folder: None, tags: vec![], visibility: None, pinned: false, task_id: None };
        let report = store.create_canvas(new, start).await?;
        let org = report.org_id.unwrap_or(org);
        assert_eq!(store.find_report_brief(owner, org, &report.id.to_string()).await?.map(|b| b.item_count), Some(4));

        let found = store.describe_refs(owner, org, &[format!("quest:{quest}"), format!("todo:{todo}"), "quest:deadbeef00".into()]).await?;
        assert!(found[&format!("quest:{quest}")].contains("Ship the drawbridge"));
        assert!(found[&format!("todo:{todo}")].contains("Oil the hinges"));
        assert!(!found.contains_key("quest:deadbeef00"));
        let short = format!("quest:{}", &quest.simple().to_string()[..10]);
        let slug: String = sqlx::query_scalar("SELECT slug FROM agent_profiles WHERE id = $1").bind(agent).fetch_one(&pool).await?;
        let batch = store.describe_refs(owner, org, &[short.clone(), format!("agent:{}", slug.to_uppercase()), format!("report:{}", report.id), "agent:nobody-here".into()]).await?;
        assert!(batch[&short].contains("Ship the drawbridge"));
        assert_eq!(batch[&format!("agent:{}", slug.to_uppercase())], "Agent Boarder");
        assert!(batch[&format!("report:{}", report.id)].starts_with("Canvas \"Castle plan\""));
        assert!(!batch.contains_key("agent:nobody-here"));
        assert!(store.describe_refs(stranger, org, &[format!("todo:{todo}")]).await?.is_empty());

        let mut quote = crate::quotes::clean(Some(&json!({ "text": "Castle plan", "source": format!("report:{}#t", report.id) }))).unwrap();
        store.expand_canvas_quote(owner, org, &mut quote, QuoteCards::Resolved).await?;
        let context = quote["context"].as_str().unwrap();
        assert!(context.starts_with("The quoted item: ref t: To-do \"Oil the hinges\""));
        assert!(context.contains("Ship the drawbridge") && context.contains(&word) && context.contains("Metric"));
        let mut channel = crate::quotes::clean(Some(&json!({ "text": "Castle plan", "source": format!("report:{}#t", report.id) }))).unwrap();
        store.expand_canvas_quote(owner, org, &mut channel, QuoteCards::RefsOnly).await?;
        let shared = channel["context"].as_str().unwrap();
        assert!(shared.starts_with(&format!("The quoted item: ref t: todo:{todo}")));
        assert!(shared.contains(&word) && shared.contains(&format!("quest:{quest}")));
        assert!(!shared.contains("Oil the hinges") && !shared.contains("Ship the drawbridge"));
        let mut hidden = crate::quotes::clean(Some(&json!({ "text": "x", "source": format!("report:{}", report.id) }))).unwrap();
        store.expand_canvas_quote(stranger, org, &mut hidden, QuoteCards::Resolved).await?;
        assert!(hidden.get("context").is_none());

        for structure in ["elements", "props", "Metric"] {
            let found = store.search_canvas_items(owner, org, structure, 10).await?;
            assert!(found.iter().filter(|h| h.id == report.id).all(|h| h.items == json!([])), "{structure} matched an item through its card JSON");
        }
        let card = store.search_canvas_items(owner, org, "Bridges", 10).await?;
        let card = card.iter().find(|h| h.id == report.id).unwrap();
        assert_eq!((card.items[0]["id"].clone(), card.items[0]["label"].clone()), (json!("u"), json!("Bridges 3")));
        let hits = store.search_canvas_items(owner, org, &word, 10).await?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].items[0]["id"], json!("n"));
        assert!(store.search_canvas_items(stranger, org, &word, 10).await?.is_empty());
        let search = store.search(owner, crate::access::Role::Write, org, &word, 10).await?;
        let hit = search.reports.iter().find(|r| r.id == report.id).unwrap();
        assert_eq!((hit.kind.as_str(), hit.items[0]["id"].clone()), ("canvas", json!("n")));

        let mark = store.add_canvas_mark(&report, owner, NewMark { item_id: Some("q".into()), body: "ship it sooner".into(), ..Default::default() }).await?;
        assert!(store.remove_canvas_mark(&report, mark.id, ReportActor::User(owner)).await?);
        assert!(!store.remove_canvas_mark(&report, mark.id, ReportActor::User(owner)).await?);
        assert!(store.canvas_marks(report.id).await?.is_empty());

        sqlx::query("DELETE FROM world_events WHERE agent_id = $1 OR payload->>'owner_user_id' = $2").bind(agent).bind(owner.to_string()).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_inbox WHERE agent_id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM world_reports WHERE owner_user_id = $1").bind(owner).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_tasks WHERE id = $1").bind(quest).execute(&pool).await?;
        sqlx::query("DELETE FROM agent_profiles WHERE id = $1").bind(agent).execute(&pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![owner, stranger]).execute(&pool).await?;
        Ok(())
    }

    #[test]
    fn ui_items_digest_and_quote_sources() {
        let raw = json!({ "items": [
            { "id": "f", "kind": "frame", "x": 0, "y": 0, "w": 900, "h": 500, "caption": "Round 1" },
            { "id": "q", "kind": "ref", "target": "quest:abcdef12", "x": 40, "y": 40 },
            { "id": "u", "kind": "ui", "x": 400, "y": 40, "caption": "Spend", "spec": { "root": "c", "elements": { "c": { "type": "Metric", "props": { "label": "Spend today", "value": "$4" } } } } },
            { "id": "r", "kind": "run", "command": "cargo test", "x": 40, "y": 300 }
        ] });
        let c = Canvas::from_raw(&raw, "agent").unwrap();
        let u = c.items.iter().find(|i| i.id == "u").unwrap();
        assert_eq!((u.w, u.h), (360.0, 280.0));
        assert!(spec_text(u.spec.as_ref().unwrap()).contains("Metric Spend today"));
        assert!(c.summary().contains("```ui\n{"));
        let resolved = HashMap::from([("quest:abcdef12".to_string(), "Quest \"Fix login\" (in progress)".to_string())]);
        let id = Uuid::nil();
        let digest = c.digest("Plan", id, &resolved, 4000);
        assert!(digest.starts_with(&format!("Canvas \"Plan\" (report:{id}) with 4 items")));
        let frame = digest.find("## frame f: Round 1").unwrap();
        assert!(frame < digest.find("ref q: Quest \"Fix login\" (in progress) (quest:abcdef12)").unwrap());
        assert!(digest.contains("ui u: Metric Spend today") && digest.contains("run r: runs `cargo test`"));
        assert!(c.digest("Plan", id, &resolved, 120).contains("more items"));
        assert_eq!(c.describe_item("q", &HashMap::new(), 100).as_deref(), Some("ref q: quest:abcdef12"));
        assert!(c.item_line("u").unwrap().starts_with("ui u: Spend"));

        let bad = json!({ "items": [{ "id": "u", "kind": "ui", "spec": { "root": "x", "elements": { "x": { "type": "Blink" } } } }] });
        assert!(Canvas::from_raw(&bad, "agent").is_err());
        assert!(Canvas::from_raw(&json!({ "items": [{ "id": "u", "kind": "ui" }] }), "agent").is_err());

        assert_eq!(quote_source("report:abcdef12#n1"), Some(("abcdef12", Some("n1"))));
        assert_eq!(quote_source("report:abcdef12"), Some(("abcdef12", None)));
        assert_eq!(quote_source("report:abc#n1"), None);
        assert_eq!(quote_source("report:abcdef12#bad id"), None);
        assert_eq!(quote_source("quest:abcdef12"), None);
    }

    fn note(text: &str) -> ItemInput {
        ItemInput { kind: Some("note".into()), text: Some(text.into()), ..Default::default() }
    }

    #[test]
    fn ops_add_connect_layout_and_validate() {
        let mut c = Canvas::default();
        let added = c.apply_ops(vec![Op::Add { items: vec![note("A"), note("B"), note("C")], edges: vec![] }], "agent").unwrap();
        assert_eq!(added.len(), 3);
        assert_eq!(c.items[1].x, c.items[0].x + c.items[0].w + GAP);
        c.apply_ops(vec![Op::Layout { ids: added.clone(), mode: "flow".into(), gap: Some(10.0), columns: None, x: Some(0.0), y: Some(0.0) }], "agent").unwrap();
        assert_eq!(c.edges.len(), 2);
        assert_eq!((c.items[2].x, c.items[2].y), (660.0, 0.0));
        c.apply_ops(vec![Op::Layout { ids: added.clone(), mode: "grid".into(), gap: Some(0.0), columns: Some(2), x: None, y: None }], "agent").unwrap();
        assert_eq!((c.items[2].x, c.items[2].y), (0.0, 200.0));
        c.apply_ops(vec![Op::Connect { edges: vec![EdgeInput { from: added[0].clone(), to: added[1].clone(), label: Some("again".into()), ..Default::default() }] }], "agent").unwrap();
        assert_eq!(c.edges.len(), 2);
        assert_eq!(c.edges[0].label.as_deref(), Some("again"));
        c.validate().unwrap();
        c.apply_ops(vec![Op::Remove { ids: vec![added[1].clone()] }], "agent").unwrap();
        assert!(c.edges.is_empty());
        assert!(c.apply_ops(vec![Op::Add { items: vec![ItemInput { kind: Some("image".into()), ..Default::default() }], edges: vec![] }], "agent").is_ok());
        assert!(c.validate().is_err());
    }

    #[test]
    fn restored_items_keep_agent_authorship_but_agents_cannot_claim_a_person() {
        let mut c = Canvas::default();
        let ops = parse_ops(&json!({ "op": "add", "items": [{ "id": "s", "kind": "sticky", "text": "hi", "by": "agent" }, { "id": "t", "kind": "sticky", "text": "me", "by": "person" }] })).unwrap();
        c.apply_ops(ops, "person").unwrap();
        assert_eq!((c.items[0].by.as_str(), c.items[1].by.as_str()), ("agent", "person"));
        let mut d = Canvas::default();
        d.apply_ops(parse_ops(&json!({ "op": "add", "items": [{ "kind": "sticky", "text": "hi", "by": "person" }] })).unwrap(), "agent").unwrap();
        assert_eq!(d.items[0].by, "agent");
        let raw = Canvas::from_raw(&json!({ "items": [{ "kind": "note", "text": "x", "by": "agent" }] }), "person").unwrap();
        assert_eq!(raw.items[0].by, "person");
    }

    #[test]
    fn refuses_bad_items_and_reads_ops() {
        let mut c = Canvas::default();
        assert!(c.apply_ops(vec![Op::Add { items: vec![ItemInput { kind: Some("html".into()), ..Default::default() }], edges: vec![] }], "agent").is_err());
        let ops = parse_ops(&json!([{ "op": "add", "items": [{ "kind": "ref", "target": "quest:12345678" }, { "kind": "sticky", "text": "hi", "color": "rose" }] }])).unwrap();
        c.apply_ops(ops, "person").unwrap();
        c.validate().unwrap();
        assert_eq!(c.items[1].by, "person");
        let bad = parse_ops(&json!({ "op": "add", "items": [{ "kind": "ref", "target": "javascript:alert(1)" }] })).unwrap();
        let mut d = Canvas::default();
        d.apply_ops(bad, "agent").unwrap();
        assert!(d.validate().is_err());
        let mut e = Canvas::default();
        e.apply_ops(parse_ops(&json!({ "op": "add", "items": [{ "kind": "sticky", "text": "x", "color": "red" }] })).unwrap(), "agent").unwrap();
        assert!(e.validate().is_err());
        assert!(parse_ops(&json!({ "op": "explode" })).is_err());
    }

    #[test]
    fn ink_is_stored_relative_and_summary_reads_in_order() {
        let mut c = Canvas::default();
        c.apply_ops(parse_ops(&json!({ "op": "add", "items": [{ "kind": "ink", "points": [[100.0, 100.0], [140.0, 180.0]] }] })).unwrap(), "person").unwrap();
        assert_eq!((c.items[0].x, c.items[0].y), (92.0, 92.0));
        assert_eq!(c.items[0].points.as_ref().unwrap()[0], [8.0, 8.0]);
        let upload = Uuid::new_v4();
        c.apply_ops(
            parse_ops(&json!({ "op": "add", "items": [
                { "kind": "image", "upload_id": upload, "caption": "the look", "x": 0, "y": 0 },
                { "kind": "note", "text": "See report:abcdef12", "x": 0, "y": 500 }
            ] }))
            .unwrap(),
            "agent",
        )
        .unwrap();
        c.validate().unwrap();
        let s = c.summary();
        assert!(s.find(&format!("![the look](/api/world/uploads/{upload})")).unwrap() < s.find("report:abcdef12").unwrap());
        assert!(c.outline().contains("ink"));
        let mut big = Canvas::default();
        let many: Vec<ItemInput> = (0..MAX_ITEMS + 1).map(|i| note(&format!("n{i}"))).collect();
        big.apply_ops(vec![Op::Add { items: many, edges: vec![] }], "agent").unwrap();
        assert!(big.validate().is_err());
    }
}
