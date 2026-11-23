use std::sync::Arc;

pub const MIN_SECRET_BYTES: usize = 4;
pub const MASK: &str = "[REDACTED]";

#[derive(Clone, Debug, Default)]
pub struct Redactor {
    values: Arc<[String]>,
}

impl Redactor {
    pub fn new<S: AsRef<str>>(values: impl IntoIterator<Item = S>) -> Self {
        let mut values: Vec<String> = values
            .into_iter()
            .map(|value| value.as_ref().to_string())
            .filter(|value| value.len() >= MIN_SECRET_BYTES)
            .collect();
        values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        Self { values: values.into() }
    }

    pub fn extended<S: AsRef<str>>(&self, more: impl IntoIterator<Item = S>) -> Self {
        let mut all: Vec<String> = self.values.to_vec();
        all.extend(more.into_iter().map(|value| value.as_ref().to_string()));
        Self::new(all)
    }

    pub fn longest(&self) -> usize {
        self.values.first().map_or(0, String::len)
    }

    pub fn values(&self) -> &[String] {
        &self.values
    }

    pub fn redact(&self, text: &str) -> String {
        self.values
            .iter()
            .fold(text.replace('\0', ""), |text, value| text.replace(value.as_str(), MASK))
    }

    pub fn clip(&self, text: &str, max_chars: usize) -> String {
        self.redact(text).chars().take(max_chars).collect()
    }

    pub fn tail(&self, text: &str, max_chars: usize) -> String {
        let redacted = self.redact(text);
        let count = redacted.chars().count();
        redacted.chars().skip(count.saturating_sub(max_chars)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-0123456789abcdef";

    #[test]
    fn a_secret_cut_by_any_limit_never_shows_a_piece_of_itself() {
        let redactor = Redactor::new([SECRET]);
        let text = format!("{}{SECRET}{}", "x".repeat(40), "y".repeat(40));
        for limit in 0..text.chars().count() + 4 {
            let clipped = redactor.clip(&text, limit);
            let tail = redactor.tail(&text, limit);
            for shown in [&clipped, &tail] {
                assert!(!shown.contains(SECRET), "limit {limit}");
                for width in MIN_SECRET_BYTES..SECRET.len() {
                    for start in 0..=SECRET.len() - width {
                        assert!(!shown.contains(&SECRET[start..start + width]), "limit {limit} leaked {}", &SECRET[start..start + width]);
                    }
                }
            }
        }
    }

    #[test]
    fn one_rule_for_short_values() {
        let redactor = Redactor::new(["abc", "abcd", ""]);
        assert_eq!(redactor.redact("abc abcd"), "abc [REDACTED]");
        assert!(Redactor::new(["a", "12", "xyz"]).values().is_empty());
    }

    #[test]
    fn longer_values_win_over_their_prefixes() {
        let redactor = Redactor::new(["abcd", "abcdefgh"]);
        assert_eq!(redactor.redact("abcdefgh"), "[REDACTED]");
    }

    #[test]
    fn unicode_and_nul_are_handled() {
        let redactor = Redactor::new(["密钥-value"]);
        assert_eq!(redactor.redact("a\0 密钥-value b"), "a [REDACTED] b");
        assert_eq!(redactor.clip("é密钥", 2), "é密");
    }
}
