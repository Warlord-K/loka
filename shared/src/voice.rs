use serde_json::Value;

pub const SPOKEN_NOTE: &str = "[voice] They said this out loud and will hear your answer read aloud. Answer for the ear: one to three short spoken sentences, plain words, no markdown, tables, code blocks, links or long lists. Say numbers, names and times the way you would say them out loud. If there is a lot to say, give the gist and offer to put the details in the chat or a thread.";

pub fn spoken(payload: &Value) -> bool {
    ["spoken", "voice"].iter().any(|k| payload.get(*k).and_then(Value::as_bool) == Some(true))
}

pub fn with_note(prompt: String, payload: &Value) -> String {
    if spoken(payload) && !prompt.contains(SPOKEN_NOTE) {
        format!("{prompt}\n\n{SPOKEN_NOTE}")
    } else {
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn voice_items_get_one_note() {
        let once = with_note("hi".into(), &json!({ "spoken": true }));
        assert!(once.ends_with(SPOKEN_NOTE));
        assert_eq!(with_note(once.clone(), &json!({ "voice": true })), once);
        assert_eq!(with_note("hi".into(), &json!({ "spoken": false })), "hi");
        assert!(!SPOKEN_NOTE.contains('\u{2014}'));
    }
}
