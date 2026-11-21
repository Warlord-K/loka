use serde_json::{json, Value};

const TEXT_CHARS: usize = 2000;
pub const CONTEXT_CHARS: usize = 4000;
const LABEL_CHARS: usize = 120;
const SOURCE_KINDS: &[&str] = &["msg:ch", "msg:", "report:", "todo:", "quest:", "agent:"];

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
}
