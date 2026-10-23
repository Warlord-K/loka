use std::path::Path;

pub const RESERVED_PREFIXES: [&str; 3] = ["WORLD_", "LOKA_", "KARNA_"];

const CONFIG_PATH: &str = "/etc/loka/config.yaml";
const LEGACY_CONFIG_PATH: &str = "/etc/karna/config.yaml";

pub fn var(suffix: &str) -> Option<String> {
    std::env::var(format!("LOKA_{suffix}"))
        .or_else(|_| std::env::var(format!("KARNA_{suffix}")))
        .ok()
}

pub fn flag(suffix: &str) -> bool {
    var(suffix).is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

pub fn reserved(name: &str) -> bool {
    RESERVED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

pub fn config_path() -> String {
    if let Ok(path) = std::env::var("CONFIG_PATH") {
        return path;
    }
    if !Path::new(CONFIG_PATH).exists() && Path::new(LEGACY_CONFIG_PATH).exists() {
        return LEGACY_CONFIG_PATH.to_string();
    }
    CONFIG_PATH.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_names_win_over_old_ones() {
        std::env::set_var("KARNA_LOKA_ENV_TEST", "old");
        assert_eq!(var("LOKA_ENV_TEST").as_deref(), Some("old"));
        std::env::set_var("LOKA_LOKA_ENV_TEST", "new");
        assert_eq!(var("LOKA_ENV_TEST").as_deref(), Some("new"));
        std::env::set_var("KARNA_LOKA_ENV_FLAG", "on");
        assert!(flag("LOKA_ENV_FLAG"));
        assert!(!flag("LOKA_ENV_MISSING"));
        assert!(reserved("LOKA_MODE") && reserved("KARNA_MODE") && reserved("WORLD_X") && !reserved("GITHUB_TOKEN"));
    }
}
