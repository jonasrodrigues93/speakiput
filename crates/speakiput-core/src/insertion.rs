// Adapted from whisrs src/daemon/injection.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmInsertion {
    Insert(String),
    Empty,
    RefusedMultilineTerminal(String),
}

#[must_use]
pub fn prepare_llm_insertion(raw: &str, is_terminal: bool) -> LlmInsertion {
    let cleaned = speakiput_llm::clean_llm_output(raw);
    if cleaned.is_empty() {
        return LlmInsertion::Empty;
    }
    if is_terminal && speakiput_llm::contains_line_break(&cleaned) {
        return LlmInsertion::RefusedMultilineTerminal(cleaned);
    }
    LlmInsertion::Insert(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_one_liner_is_unwrapped() {
        assert_eq!(
            prepare_llm_insertion("```bash\necho safe\n```", true),
            LlmInsertion::Insert("echo safe".into())
        );
    }

    #[test]
    fn multiline_llm_output_is_refused_only_at_terminals() {
        let text = "cd /tmp\necho hello";
        assert_eq!(
            prepare_llm_insertion(text, true),
            LlmInsertion::RefusedMultilineTerminal(text.into())
        );
        assert_eq!(
            prepare_llm_insertion(text, false),
            LlmInsertion::Insert(text.into())
        );
    }
}
