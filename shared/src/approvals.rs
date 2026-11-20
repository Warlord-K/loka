use crate::world::WorldRule;

pub const ENV: &str = "LOKA_APPROVED_ACTIONS";
pub const SCOPE: &str = "allow";
const MAX_ACTIONS: usize = 40;
const MAX_CHARS: usize = 600;

pub fn from_rules(rules: &[WorldRule]) -> Vec<String> {
    rules
        .iter()
        .filter(|rule| rule.enabled && rule.scope == SCOPE)
        .map(|rule| rule.body.trim().chars().take(MAX_CHARS).collect::<String>())
        .filter(|body| !body.is_empty())
        .take(MAX_ACTIONS)
        .collect()
}

pub fn classifier_entry(action: &str) -> String {
    format!(
        "Owner Approved: the person who owns this agent saved this as a standing approval, so it counts as their explicit request whenever the task needs it: {}. This approval covers only the action it names; every rule about credentials, sensitive data and exfiltration still applies to what the action touches.",
        action.trim().trim_end_matches('.')
    )
}

pub fn parse_env(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value)
        .unwrap_or_default()
        .into_iter()
        .map(|action| action.trim().chars().take(MAX_CHARS).collect::<String>())
        .filter(|action| !action.is_empty())
        .take(MAX_ACTIONS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn rule(scope: &str, body: &str, enabled: bool) -> WorldRule {
        WorldRule { id: Uuid::new_v4(), user_id: None, scope: scope.into(), body: body.into(), enabled, position: 0, created_at: Utc::now(), updated_at: Utc::now() }
    }

    #[test]
    fn only_enabled_allow_rules_become_approvals() {
        let rules = [rule("allow", " Delete folders under /tmp the quest made ", true), rule("allow", "off", false), rule("work", "no comments", true)];
        assert_eq!(from_rules(&rules), vec!["Delete folders under /tmp the quest made".to_string()]);
        assert_eq!(parse_env(&serde_json::to_string(&from_rules(&rules)).unwrap()), from_rules(&rules));
        assert!(parse_env("not json").is_empty());
        assert!(classifier_entry("Run scripts in ./scripts.").contains("Run scripts in ./scripts. This approval"));
    }
}
