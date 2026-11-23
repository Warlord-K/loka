use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn kept(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

impl RunRequest {
    pub fn asked(from: &str, text: Option<&str>) -> Self {
        RunRequest { from: kept(Some(from)), text: kept(text), ..Default::default() }
    }

    pub fn task(description: Option<&str>, brief: Option<&str>) -> Self {
        RunRequest { text: kept(description), brief: kept(brief), ..Default::default() }
    }

    pub fn fix(findings: &str) -> Self {
        RunRequest { from: Some("Reviewer".into()), findings: kept(Some(findings)), ..Default::default() }
    }

    pub fn review(focus: Option<&str>) -> Self {
        RunRequest { focus: kept(focus), ..Default::default() }
    }

    pub fn retried(mut self) -> Self {
        self.retry += 1;
        self
    }

    pub fn value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn from_value(value: Option<&Value>) -> Self {
        value.and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_keep_only_what_was_asked() {
        assert_eq!(RunRequest::asked(" Tara ", Some("  ")).value(), json!({ "from": "Tara" }));
        assert_eq!(RunRequest::task(Some(" Fix it "), Some("Do the thing\n\nwith care")).value(), json!({ "text": "Fix it", "brief": "Do the thing\n\nwith care" }));
        assert_eq!(RunRequest::task(None, None).value(), json!({}));
        assert_eq!(RunRequest::fix("[{\"issue\": \"x\"}]").retried().retried().value(), json!({ "from": "Reviewer", "findings": "[{\"issue\": \"x\"}]", "retry": 2 }));
        let back = RunRequest::from_value(Some(&RunRequest::review(Some("tests")).retried().value()));
        assert_eq!(back, RunRequest { focus: Some("tests".into()), retry: 1, ..Default::default() });
        assert_eq!(RunRequest::from_value(Some(&json!("junk"))), RunRequest::default());
    }
}
