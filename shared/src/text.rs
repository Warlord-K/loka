pub fn clip(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub fn clip_with_ellipsis(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        text.to_string()
    } else {
        format!("{}…", clip(text, max_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_on_character_boundaries() {
        let text = format!("{}— rest", "a".repeat(119));
        assert_eq!(clip(&text, 120), "a".repeat(119));
        assert_eq!(clip_with_ellipsis(&text, 120), format!("{}…", "a".repeat(119)));
        assert_eq!(clip("short", 120), "short");
        assert_eq!(clip("✓✓", 4), "✓");
        assert_eq!(clip("é", 1), "");
    }
}
