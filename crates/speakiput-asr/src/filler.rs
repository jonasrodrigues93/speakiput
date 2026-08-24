// Substantially derived from whisrs crates/filler-remove at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

use std::sync::LazyLock;

use regex::Regex;

const DEFAULT_FILLER_PATTERNS: &[&str] = &[
    r"\bum\b,?\s*",
    r"\buh\b,?\s*",
    r"\blike,\s*",
    r"\byou know,?\s*",
    r"\bbasically,?\s*",
    r"\bactually,?\s*",
    r"\bI mean,?\s*",
    r"\bsort of\b",
    r"\bkind of\b",
];

static BUILTIN_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    DEFAULT_FILLER_PATTERNS
        .iter()
        .map(|pattern| Regex::new(&format!("(?i){pattern}")).expect("valid built-in filler"))
        .collect()
});
static SPACE_COLLAPSE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r" {2,}").expect("valid space regex"));

pub fn remove_filler_words(text: &str, custom_words: &[String]) -> String {
    let custom = custom_words
        .iter()
        .filter(|word| !word.trim().is_empty())
        .map(|word| Regex::new(&format!(r"(?i)\b{},?\s*", regex::escape(word.trim()))))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();
    let patterns = if custom.is_empty() {
        BUILTIN_PATTERNS.as_slice()
    } else {
        custom.as_slice()
    };
    let mut cleaned = text.to_owned();
    for pattern in patterns {
        cleaned = pattern.replace_all(&cleaned, "").into_owned();
    }
    cleaned = remove_stutters(&cleaned);
    SPACE_COLLAPSE_RE
        .replace_all(cleaned.trim(), " ")
        .into_owned()
}

fn remove_stutters(text: &str) -> String {
    let mut result = Vec::new();
    for word in text.split_whitespace() {
        if result
            .last()
            .is_none_or(|previous: &&str| !previous.eq_ignore_ascii_case(word))
        {
            result.push(word);
        }
    }
    result.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_fillers_and_repeated_words() {
        assert_eq!(
            remove_filler_words("um I I actually went home", &[]),
            "I went home"
        );
    }

    #[test]
    fn supports_custom_non_english_fillers() {
        assert_eq!(
            remove_filler_words("é tipo tipo um teste", &["tipo".into()]),
            "é um teste"
        );
    }
}
