use std::str::FromStr;

use cron::Schedule as CronSchedule;

pub fn normalize(expr: &str) -> String {
    let trimmed = expr.trim();
    if trimmed.split_whitespace().count() == 5 {
        format!("0 {trimmed}")
    } else {
        trimmed.to_string()
    }
}

pub fn validate(expr: &str) -> Result<(), String> {
    let normalized = normalize(expr);
    CronSchedule::from_str(&normalized)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_field_patterns_parse_after_normalization() {
        assert!(validate("0 9 * * 1,4").is_ok());
        assert!(validate("0 */5 * * *").is_ok());
        for preset in [
            "* * * * *",
            "*/5 * * * *",
            "*/15 * * * *",
            "*/30 * * * *",
            "0 * * * *",
            "0 */2 * * *",
            "0 */4 * * *",
            "0 */6 * * *",
            "0 */12 * * *",
            "0 0 * * *",
            "0 9 * * *",
            "0 9 * * 1",
            "0 9 * * 1-5",
        ] {
            assert!(validate(preset).is_ok(), "preset failed: {preset}");
        }
    }

    #[test]
    fn six_and_seven_field_patterns_pass_through() {
        assert!(validate("0 0 9 * * Mon,Thu").is_ok());
        assert!(validate("0 0 9 * * Mon,Thu *").is_ok());
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(normalize("  0 9 * * 1,4  "), "0 0 9 * * 1,4");
    }

    #[test]
    fn invalid_expressions_are_rejected() {
        assert!(validate("not a cron").is_err());
        assert!(validate("0 99 * * *").is_err());
    }
}
