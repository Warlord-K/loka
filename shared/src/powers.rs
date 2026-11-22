pub const MANAGE_AGENTS: &str = "manage_agents";
pub const ASSIGN_OTHERS: &str = "assign_others";
pub const SCHEDULES: &str = "schedules";
pub const INSTALL: &str = "install";
pub const OVERSEE: &str = "oversee";
pub const MANAGE_POWERS: &str = "manage_powers";
pub const MANAGE_QUESTS: &str = "manage_quests";
pub const FEEDS: &str = "feeds";
pub const WORLDBUILD: &str = "worldbuild";
pub const TEAMS: &str = "teams";
pub const GOVERN: &str = "govern";

pub const ALL: &[(&str, &str)] = &[
    (MANAGE_AGENTS, "create new agents, change agents' model, CLI, persona and look, and move agents (or yourself) between execution devices"),
    (ASSIGN_OTHERS, "create tasks for other agents"),
    (SCHEDULES, "set up routines that wake an agent on a schedule"),
    (INSTALL, "install skills and MCP servers, and equip installed ones on other agents"),
    (OVERSEE, "follow up on and request reviews of other agents' task sessions, and see who holds which environment keys"),
    (MANAGE_QUESTS, "cancel or delete quests that are stuck or no longer needed, several at once, stopping their running work; other agents' quests also need oversee"),
    (FEEDS, "watch RSS or Atom feeds and receive webhooks such as Alertmanager alerts, which wake an agent when something new arrives"),
    (TEAMS, "group your person's agents into teams that share a channel, change or remove those teams, and give quests to a team"),
    (WORLDBUILD, "grow your person's Loka with regions, portals, characters and ambience"),
    (GOVERN, "pause and resume your person's agents and set their daily budgets and approval modes, when your person asks"),
    (MANAGE_POWERS, "grant or revoke powers for other agents, change your owner's rules, and share environment keys between their agents"),
];

pub fn is_power(name: &str) -> bool {
    ALL.iter().any(|(p, _)| *p == name)
}

pub fn normalize(names: &[String]) -> Vec<String> {
    ALL.iter()
        .map(|(p, _)| *p)
        .filter(|p| names.iter().any(|n| n.trim() == *p))
        .map(String::from)
        .collect()
}

pub fn held(revoked: &[String]) -> Vec<&'static str> {
    ALL.iter().map(|(p, _)| *p).filter(|p| !revoked.iter().any(|r| r == p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_unknown_and_dedupes() {
        let got = normalize(&["install".into(), "fly".into(), " install ".into(), "oversee".into()]);
        assert_eq!(got, vec!["install".to_string(), "oversee".to_string()]);
    }

    #[test]
    fn held_is_everything_minus_revoked() {
        let held = held(&["schedules".into()]);
        assert!(!held.contains(&SCHEDULES));
        assert_eq!(held.len(), ALL.len() - 1);
    }
}
