use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

pub const MAX_BYTES: usize = 32 * 1024;
pub const MAX_ELEMENTS: usize = 80;
pub const MAX_DEPTH: usize = 12;

const ACTIONS: &[&str] = &["reply", "submit", "setState", "pushState", "removeState", "validateForm"];

struct Component {
    name: &'static str,
    strings: &'static [&'static str],
    arrays: &'static [&'static str],
    children: bool,
}

const COMPONENTS: &[Component] = &[
    Component { name: "Card", strings: &[], arrays: &[], children: true },
    Component { name: "Stack", strings: &[], arrays: &[], children: true },
    Component { name: "Heading", strings: &["text"], arrays: &[], children: false },
    Component { name: "Text", strings: &["text"], arrays: &[], children: false },
    Component { name: "Metric", strings: &["label"], arrays: &[], children: false },
    Component { name: "Progress", strings: &[], arrays: &[], children: false },
    Component { name: "Badge", strings: &["label"], arrays: &[], children: false },
    Component { name: "Table", strings: &[], arrays: &["columns", "rows"], children: false },
    Component { name: "Chart", strings: &[], arrays: &["labels", "series"], children: false },
    Component { name: "List", strings: &[], arrays: &["items"], children: false },
    Component { name: "Divider", strings: &[], arrays: &[], children: false },
    Component { name: "Ref", strings: &["ref"], arrays: &[], children: false },
    Component { name: "Link", strings: &["label", "href"], arrays: &[], children: false },
    Component { name: "Button", strings: &["label"], arrays: &[], children: false },
    Component { name: "TextInput", strings: &["label"], arrays: &[], children: false },
    Component { name: "Select", strings: &["label"], arrays: &["options"], children: false },
    Component { name: "Checkbox", strings: &["label"], arrays: &[], children: false },
];

pub const GUIDE: &str = "Show your owner a small interactive card in your chat (json-render spec). Use it only when a layout clearly beats prose: a status dashboard, a chart of numbers over time or across groups, a comparison table, or a short form or set of buttons you need answered. When they ask for a chart or graph, this is it: bar to compare groups, line or area for change over time, pie only for a few parts of one whole. Plain replies stay the default. \
The card appears as its own message before your reply; summary is the plain text shown in notifications and phones and quoted back to you when they answer. \
spec = {\"root\": \"<key>\", \"elements\": {\"<key>\": {\"type\": \"...\", \"props\": {...}, \"children\": [\"<key>\", ...], \"on\": {\"press\": {\"action\": \"...\", \"params\": {...}}}, \"visible\": {\"$state\": \"/path\", \"eq\": \"x\"}}}, \"state\": {...}}. Max 80 elements, 32 KB. \
Components (props; ? = optional): Card(title?, subtitle?; children), Stack(direction? row|column, gap? sm|md|lg; children), Heading(text), Text(text markdown, tone? normal|muted|ok|warn|danger), Metric(label, value string|number, hint?, trend? up|down|flat), Progress(label?, value 0-100), Badge(label, tone?), Table(columns string[] up to 8, rows array of arrays up to 50), Chart(kind? bar|line|area|pie, title?, labels string[] up to 40, series [{name, values number[] one per label}] up to 5 (pie takes one), unit? like $ or %, stacked?), List(items markdown string[] up to 30, ordered?), Divider(), Ref(ref: quest:<id> | report:<id> | todo:<id> | msg:<id> | practice:<id> | challenge:<id> | course:<id> | setup:<key>, shows a live card), Link(label, href https), Button(label, variant? primary|secondary|danger; on.press), TextInput(label, value {\"$bindState\": \"/form/x\"}, placeholder?, multiline?), Select(label, value {\"$bindState\": ...}, options string[]), Checkbox(label, checked {\"$bindState\": ...}). \
Actions for on.press: reply {text} sends that text to you as their answer (text may be {\"$template\": \"Use ${/form/model}\"}); submit {label?} sends every value in state to you as a list; setState {statePath, value} changes local state (tabs, toggles) without sending. A card takes one answer, then locks. Seed form defaults in state. For a lasting dashboard or a choice inside a write-up, put the same spec in a report as a fenced ui code block instead.";

fn fail(path: &str, why: &str) -> Result<()> {
    bail!("{path}: {why}")
}

fn check_on(key: &str, on: &Value) -> Result<()> {
    let Some(events) = on.as_object() else {
        return fail(&format!("elements.{key}.on"), "must be an object like {\"press\": {\"action\": \"reply\", \"params\": {\"text\": \"...\"}}}");
    };
    for (event, binding) in events {
        let list = match binding {
            Value::Array(items) => items.iter().collect::<Vec<_>>(),
            other => vec![other],
        };
        for b in list {
            let action = b.get("action").and_then(Value::as_str).unwrap_or("");
            if !ACTIONS.contains(&action) {
                return fail(
                    &format!("elements.{key}.on.{event}"),
                    &format!("action must be one of {}", ACTIONS.join(", ")),
                );
            }
        }
    }
    Ok(())
}

const CHART_KINDS: &[&str] = &["bar", "line", "area", "pie"];
pub const CHART_MAX_LABELS: usize = 40;
pub const CHART_MAX_SERIES: usize = 5;

fn check_chart(path: &str, props: &Value) -> Result<()> {
    let kind = match props.get("kind") {
        None | Some(Value::Null) => "bar",
        Some(Value::String(k)) if CHART_KINDS.contains(&k.as_str()) => k.as_str(),
        Some(_) => return fail(&format!("{path}.props.kind"), &format!("must be one of {}", CHART_KINDS.join(", "))),
    };
    let labels = props.get("labels").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    if labels == 0 || labels > CHART_MAX_LABELS || !props["labels"].as_array().is_some_and(|l| l.iter().all(Value::is_string)) {
        return fail(&format!("{path}.props.labels"), &format!("must hold 1 to {CHART_MAX_LABELS} strings"));
    }
    let series = props.get("series").and_then(Value::as_array).cloned().unwrap_or_default();
    let most = if kind == "pie" { 1 } else { CHART_MAX_SERIES };
    if series.is_empty() || series.len() > most {
        return fail(&format!("{path}.props.series"), &format!("must hold 1 to {most} series for a {kind} chart"));
    }
    for (i, one) in series.iter().enumerate() {
        let at = format!("{path}.props.series[{i}]");
        if !one.get("name").is_some_and(Value::is_string) {
            return fail(&format!("{at}.name"), "is required and must be a string");
        }
        let values = one.get("values").and_then(Value::as_array).cloned().unwrap_or_default();
        if values.len() != labels || !values.iter().all(|v| v.as_f64().is_some_and(f64::is_finite)) {
            return fail(&format!("{at}.values"), &format!("must hold {labels} numbers, one per label"));
        }
        if kind == "pie" && values.iter().any(|v| v.as_f64().is_some_and(|n| n < 0.0)) {
            return fail(&format!("{at}.values"), "cannot be negative in a pie chart");
        }
    }
    Ok(())
}

pub fn normalize(spec: &Value) -> Result<Value> {
    let Some(obj) = spec.as_object() else { bail!("spec must be an object with root and elements") };
    let root = obj.get("root").and_then(Value::as_str).unwrap_or("").to_string();
    if root.is_empty() {
        bail!("spec.root must name one of the elements");
    }
    let Some(elements) = obj.get("elements").and_then(Value::as_object) else {
        bail!("spec.elements must be an object keyed by element id")
    };
    if elements.is_empty() || elements.len() > MAX_ELEMENTS {
        bail!("spec.elements must hold 1 to {MAX_ELEMENTS} elements, got {}", elements.len());
    }
    if !elements.contains_key(&root) {
        bail!("spec.root \"{root}\" is not in elements");
    }
    let kinds: HashMap<&str, &Component> = COMPONENTS.iter().map(|c| (c.name, c)).collect();
    let mut out = Map::new();
    for (key, el) in elements {
        let path = format!("elements.{key}");
        let Some(el) = el.as_object() else { bail!("{path} must be an object") };
        let kind = el.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(def) = kinds.get(kind) else {
            bail!(
                "{path}.type \"{kind}\" is not a component; use one of {}",
                COMPONENTS.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
            );
        };
        let props = match el.get("props") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(Value::Object(p)) => Value::Object(p.clone()),
            Some(_) => bail!("{path}.props must be an object"),
        };
        for name in def.strings {
            if !props.get(*name).is_some_and(Value::is_string) {
                fail(&format!("{path}.props.{name}"), "is required and must be a string")?;
            }
        }
        for name in def.arrays {
            if !props.get(*name).is_some_and(Value::is_array) {
                fail(&format!("{path}.props.{name}"), "is required and must be an array")?;
            }
        }
        if kind == "Metric" && !props.get("value").is_some_and(|v| v.is_string() || v.is_number()) {
            fail(&format!("{path}.props.value"), "is required and must be a string or number")?;
        }
        if kind == "Chart" {
            check_chart(&path, &props)?;
        }
        if kind == "Progress" && !props.get("value").and_then(Value::as_f64).is_some_and(|v| (0.0..=100.0).contains(&v)) {
            fail(&format!("{path}.props.value"), "is required and must be a number from 0 to 100")?;
        }
        let children = match el.get("children") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|c| c.as_str().map(str::to_string).ok_or_else(|| anyhow::anyhow!("{path}.children must list element keys")))
                .collect::<Result<Vec<_>>>()?,
            Some(_) => bail!("{path}.children must be an array of element keys"),
        };
        if !children.is_empty() && !def.children {
            bail!("{path}: {kind} has no children; only Card and Stack hold children");
        }
        for child in &children {
            if !elements.contains_key(child) {
                bail!("{path}.children names \"{child}\", which is not in elements");
            }
        }
        if let Some(on) = el.get("on") {
            check_on(key, on)?;
        }
        let mut clean = Map::new();
        clean.insert("type".into(), json!(kind));
        clean.insert("props".into(), props);
        clean.insert("children".into(), json!(children));
        for extra in ["on", "visible", "repeat", "watch"] {
            if let Some(v) = el.get(extra).filter(|v| !v.is_null()) {
                clean.insert(extra.into(), v.clone());
            }
        }
        out.insert(key.clone(), Value::Object(clean));
    }
    let mut seen = HashSet::new();
    let mut stack = vec![(root.clone(), 0usize, vec![root.clone()])];
    while let Some((key, depth, trail)) = stack.pop() {
        if depth > MAX_DEPTH {
            bail!("spec nests deeper than {MAX_DEPTH} levels");
        }
        seen.insert(key.clone());
        for child in out[&key]["children"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if trail.iter().any(|t| t == child) {
                bail!("elements.{key}.children loops back to \"{child}\"");
            }
            let mut next = trail.clone();
            next.push(child.to_string());
            stack.push((child.to_string(), depth + 1, next));
        }
    }
    let mut normalized = Map::new();
    normalized.insert("root".into(), json!(root));
    normalized.insert("elements".into(), Value::Object(out));
    if let Some(state) = obj.get("state").filter(|v| !v.is_null()) {
        if !state.is_object() {
            bail!("spec.state must be an object");
        }
        normalized.insert("state".into(), state.clone());
    }
    let normalized = Value::Object(normalized);
    let size = normalized.to_string().len();
    if size > MAX_BYTES {
        bail!("spec is {size} bytes; keep it under {MAX_BYTES}");
    }
    if seen.len() < elements.len() {
        bail!("some elements are never reached from root; list them in a Card or Stack's children or drop them");
    }
    Ok(normalized)
}

pub fn check_markdown(body: &str) -> Result<usize> {
    let mut found = 0;
    let mut open: Option<(String, bool, usize, Vec<&str>)> = None;
    for (i, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        let fence = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f));
        match (&mut open, fence) {
            (None, Some(f)) => {
                let marker: String = trimmed.chars().take_while(|c| f.starts_with(*c)).collect();
                let info = trimmed[marker.len()..].trim();
                let is_ui = info.split_whitespace().next() == Some("ui");
                open = Some((marker, is_ui, i + 1, Vec::new()));
            }
            (Some((marker, is_ui, start, lines)), _) => {
                if trimmed.starts_with(marker.as_str()) && trimmed[marker.len()..].trim().is_empty() {
                    if *is_ui {
                        found += 1;
                        check_block(found, *start, &lines.join("\n"))?;
                    }
                    open = None;
                } else {
                    lines.push(line);
                }
            }
            (None, None) => {}
        }
    }
    if let Some((_, true, start, lines)) = open {
        found += 1;
        check_block(found, start, &lines.join("\n"))?;
    }
    Ok(found)
}

fn check_block(n: usize, line: usize, text: &str) -> Result<()> {
    let spec: Value = serde_json::from_str(text).map_err(|e| anyhow::anyhow!("ui block {n} (line {line}) is not valid JSON: {e}"))?;
    normalize(&spec).map_err(|e| anyhow::anyhow!("ui block {n} (line {line}): {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Value {
        json!({
            "root": "c",
            "elements": {
                "c": { "type": "Card", "props": { "title": "Pick" }, "children": ["m", "s", "b"] },
                "m": { "type": "Metric", "props": { "label": "Spend", "value": 4.2 } },
                "s": { "type": "Select", "props": { "label": "Model", "value": { "$bindState": "/form/model" }, "options": ["a", "b"] } },
                "b": { "type": "Button", "props": { "label": "Go" }, "on": { "press": { "action": "submit", "params": {} } } }
            },
            "state": { "form": { "model": "a" } }
        })
    }

    #[test]
    fn fills_defaults_and_keeps_bindings() {
        let out = normalize(&form()).unwrap();
        assert_eq!(out["elements"]["m"]["children"], json!([]));
        assert_eq!(out["elements"]["b"]["on"]["press"]["action"], "submit");
        assert_eq!(out["state"]["form"]["model"], "a");
    }

    #[test]
    fn charts_need_one_number_per_label() {
        let chart = |props: Value| json!({ "root": "c", "elements": { "c": { "type": "Chart", "props": props } } });
        let good = chart(json!({ "kind": "line", "labels": ["Mon", "Tue"], "series": [{ "name": "Spend", "values": [1.5, 2] }], "unit": "$" }));
        assert!(normalize(&good).is_ok());
        assert!(normalize(&chart(json!({ "labels": ["a"], "series": [{ "name": "n", "values": [1] }] }))).is_ok());
        let short = chart(json!({ "labels": ["a", "b"], "series": [{ "name": "n", "values": [1] }] }));
        assert!(normalize(&short).unwrap_err().to_string().contains("one per label"));
        let kind = chart(json!({ "kind": "radar", "labels": ["a"], "series": [{ "name": "n", "values": [1] }] }));
        assert!(normalize(&kind).unwrap_err().to_string().contains("props.kind"));
        let pie = chart(json!({ "kind": "pie", "labels": ["a"], "series": [{ "name": "n", "values": [1] }, { "name": "m", "values": [2] }] }));
        assert!(normalize(&pie).unwrap_err().to_string().contains("1 to 1 series"));
        let text = chart(json!({ "labels": ["a"], "series": [{ "name": "n", "values": ["1"] }] }));
        assert!(normalize(&text).is_err());
        assert!(GUIDE.contains("Chart("));
    }

    #[test]
    fn checks_ui_blocks_in_markdown() {
        let good = format!("# Title\n\n```ui\n{}\n```\n\n```js\nnot json\n```\n", form());
        assert_eq!(check_markdown(&good).unwrap(), 1);
        let mut bad = form();
        bad["elements"]["m"]["type"] = json!("Script");
        let body = format!("intro\n\n```ui\n{}\n```\n\n```ui\n{bad}\n```\n", form());
        let err = check_markdown(&body).unwrap_err().to_string();
        assert!(err.starts_with("ui block 2 (line 7)"), "{err}");
        assert!(check_markdown("~~~ui\n{oops\n~~~").unwrap_err().to_string().contains("not valid JSON"));
        assert_eq!(check_markdown("no blocks here").unwrap(), 0);
    }

    #[test]
    fn rejects_bad_specs_with_a_reason() {
        let mut bad = form();
        bad["elements"]["m"]["type"] = json!("Script");
        assert!(normalize(&bad).unwrap_err().to_string().contains("not a component"));
        let mut bad = form();
        bad["elements"]["b"]["on"]["press"]["action"] = json!("fetch");
        assert!(normalize(&bad).unwrap_err().to_string().contains("action must be one of"));
        let mut bad = form();
        bad["elements"]["c"]["children"] = json!(["m", "zz"]);
        assert!(normalize(&bad).unwrap_err().to_string().contains("\"zz\""));
        let mut bad = form();
        bad["elements"]["s"]["children"] = json!(["c"]);
        assert!(normalize(&bad).is_err());
        let mut bad = form();
        bad["elements"]["c"]["children"] = json!(["m", "s", "b", "c"]);
        assert!(normalize(&bad).unwrap_err().to_string().contains("loops back"));
        let mut bad = form();
        bad["elements"]["m"]["props"] = json!({ "label": "x" });
        assert!(normalize(&bad).unwrap_err().to_string().contains("props.value"));
        let mut bad = form();
        bad["elements"]["c"]["children"] = json!(["m", "s"]);
        assert!(normalize(&bad).unwrap_err().to_string().contains("never reached"));
    }
}
