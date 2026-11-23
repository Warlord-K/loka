use serde_json::{json, Value};
use uuid::Uuid;

use crate::{access::{Role, Viewer}, world::WorldStore};

pub const TEXT_CHARS: usize = 2000;
pub const CONTEXT_CHARS: usize = 4000;
const LABEL_CHARS: usize = 120;
const SOURCE_KINDS: &[&str] = &["msg:ch", "msg:", "report:", "todo:", "quest:", "agent:", "practice:", "team:", "list:", "challenge:", "course:"];

fn capped(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(max).collect())
}

fn source(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    let kind = SOURCE_KINDS.iter().find(|kind| raw.starts_with(*kind))?;
    if *kind == "report:" && raw.contains('#') {
        return crate::canvas::quote_source(raw).map(|_| raw.to_string());
    }
    let id = &raw[kind.len()..];
    if matches!(*kind, "team:" | "list:" | "challenge:" | "course:") {
        return (id.len() == 36 && Uuid::parse_str(id).is_ok()).then(|| raw.to_string());
    }
    if *kind == "practice:" && id.chars().filter(|c| *c != '-').count() < crate::practice::MIN_PREFIX {
        return None;
    }
    let valid = !id.is_empty()
        && id.len() <= 36
        && if *kind == "msg:ch" { id.chars().all(|c| c.is_ascii_digit()) } else { id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') };
    valid.then(|| raw.to_string())
}

pub fn clean(raw: Option<&Value>) -> Option<Value> {
    let raw = raw?.as_object()?;
    let text = capped(raw.get("text"), TEXT_CHARS)?;
    let mut quote = json!({ "text": text });
    if let Some(label) = capped(raw.get("label"), LABEL_CHARS) {
        quote["label"] = json!(label);
    }
    if let Some(source) = source(raw.get("source")) {
        quote["source"] = json!(source);
    }
    if let Some(context) = capped(raw.get("context"), CONTEXT_CHARS).filter(|c| c != &text) {
        quote["context"] = json!(context);
    }
    Some(quote)
}

impl WorldStore {
    pub async fn clean_quote(&self, viewer: &Viewer, raw: Option<&Value>) -> anyhow::Result<Option<Value>> {
        let Some(mut quote) = clean(raw) else { return Ok(None) };
        let reference = quote.get("source").and_then(Value::as_str).and_then(|source| source.split_once(':'));
        let visible = match reference {
            Some(("msg", id)) => match self.resolve_message(id, None).await? {
                Some(message) if message.task_id.is_some() => match message.reference.parse() {
                    Ok(message_id) => self.sees_message_as(message_id, viewer).await?,
                    Err(_) => false,
                },
                _ => true,
            },
            Some(("team", id)) => self.team_visible_to(viewer.user, id.parse()?).await?.is_some(),
            Some(("list", id)) => self
                .item_for_viewer(id.parse()?, viewer.user, viewer.org(), viewer.role == Role::Admin)
                .await?
                .is_some_and(|item| matches!(item.kind.as_str(), "list" | "timer")),
            Some((kind @ ("challenge" | "course"), id)) => self.academy_item_for(viewer.user, viewer.org(), kind, id.parse()?).await?.is_some(),
            _ => true,
        };
        if !visible {
            if let Some(fields) = quote.as_object_mut() {
                fields.remove("source");
            }
        }
        Ok(Some(quote))
    }
}

pub fn render(payload: &Value) -> String {
    let Some(quote) = payload.get("quote").filter(|q| q.is_object()) else {
        return String::new();
    };
    let field = |key: &str| quote.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
    let Some(text) = field("text") else {
        return String::new();
    };
    let from = match (field("label"), field("source")) {
        (Some(label), Some(source)) => format!(" from {} ({source})", crate::outside::one_line(label, LABEL_CHARS)),
        (Some(label), None) => format!(" from {}", crate::outside::one_line(label, LABEL_CHARS)),
        (None, Some(source)) => format!(" from {source}"),
        (None, None) => String::new(),
    };
    let block = |text: &str| crate::outside::defused(text).lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
    let mut out = format!("(they selected and quoted this{from}; their message is about it)\n{}\n", block(text));
    if let Some(context) = field("context") {
        out.push_str(&format!("(the text around the quote, for context)\n{}\n", block(context)));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_are_capped_checked_and_rendered() {
        let raw = json!({ "text": " the cache misses ", "label": "Forge's message", "source": "msg:ch42", "context": "Huge win. the cache misses dropped." });
        let quote = clean(Some(&raw)).unwrap();
        assert_eq!(quote["text"], "the cache misses");
        assert_eq!(quote["source"], "msg:ch42");
        let rendered = render(&json!({ "quote": quote }));
        assert!(rendered.contains("from Forge's message (msg:ch42)") && rendered.contains("> the cache misses") && rendered.contains("> Huge win."));
        assert!(clean(Some(&json!({ "text": "  " }))).is_none());
        assert!(clean(Some(&json!({ "text": "x", "source": "https://evil" }))).unwrap().get("source").is_none());
        assert!(clean(Some(&json!({ "text": "x", "source": "msg:ch4a" }))).unwrap().get("source").is_none());
        assert_eq!(clean(Some(&json!({ "text": "a".repeat(5000) }))).unwrap()["text"].as_str().unwrap().len(), TEXT_CHARS);
        assert_eq!(render(&json!({})), "");
        assert_eq!(clean(Some(&json!({ "text": "x", "source": "report:abcdef12#i1a2" }))).unwrap()["source"], "report:abcdef12#i1a2");
        assert!(clean(Some(&json!({ "text": "x", "source": "report:abcdef12#bad item" }))).unwrap().get("source").is_none());
    }

    #[test]
    fn practice_attempts_are_quote_sources_with_their_check_as_text() {
        assert_eq!(clean(Some(&json!({ "text": "x", "source": "practice:abcdef12" }))).unwrap()["source"], "practice:abcdef12");
        let lesson = format!("challenge:{}", Uuid::nil());
        assert_eq!(clean(Some(&json!({ "text": "x", "source": lesson }))).unwrap()["source"], json!(lesson));
        assert!(clean(Some(&json!({ "text": "x", "source": "course:abc123" }))).unwrap().get("source").is_none(), "course refs take a full id");
        assert!(clean(Some(&json!({ "text": "x", "source": "practice:abc123" }))).unwrap().get("source").is_none());
        assert!(clean(Some(&json!({ "text": "x", "source": "practice:abcdefgh" }))).unwrap().get("source").is_none());
        let check = crate::practice::Check::new("Even spacing", "Lift at the end", "Write it again", Some(80.0)).unwrap();
        assert_eq!(
            crate::practice::quote_text("japanese", 3, Some(&check)),
            "Japanese practice, attempt 3, score 80/100. Right: Even spacing Fix: Lift at the end Next: Write it again"
        );
        assert_eq!(crate::practice::quote_text("life-drawing", 1, None), "Life Drawing practice, attempt 1 (waiting for the coach)");
    }

    #[test]
    fn team_and_list_sources_require_full_uuids() {
        let id = Uuid::new_v4();
        for kind in ["team", "list"] {
            let reference = format!("{kind}:{id}");
            assert_eq!(clean(Some(&json!({ "text": "x", "source": reference }))).unwrap()["source"], reference);
            for invalid in ["abcdef12".to_string(), id.simple().to_string(), format!("{id}#extra")] {
                let source = format!("{kind}:{invalid}");
                assert!(clean(Some(&json!({ "text": "x", "source": source }))).unwrap().get("source").is_none());
            }
        }
    }
}
