// Copied from whisrs crates/prompt-echo at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

fn normalize(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut previous_space = true;
    for character in input.chars() {
        if character.is_alphanumeric() {
            for lowercase in character.to_lowercase() {
                output.push(lowercase);
            }
            previous_space = false;
        } else if !previous_space {
            output.push(' ');
            previous_space = true;
        }
    }
    if output.ends_with(' ') {
        output.pop();
    }
    output
}

#[must_use]
pub fn is_prompt_echo(response: &str, prompt: &str) -> bool {
    let response = normalize(response);
    let prompt = normalize(prompt);
    if response.chars().count() < 8 || prompt.is_empty() {
        return false;
    }
    if prompt.contains(&response) {
        return true;
    }
    let response_words = response.split_whitespace().collect::<Vec<_>>();
    let prompt_words = prompt.split_whitespace().collect::<Vec<_>>();
    if response_words.len() < 6 {
        return false;
    }
    let max_run = longest_common_word_run(&response_words, &prompt_words);
    max_run >= 6 && max_run.saturating_mul(10) >= response_words.len().saturating_mul(7)
}

fn longest_common_word_run(first: &[&str], second: &[&str]) -> usize {
    if first.is_empty() || second.is_empty() {
        return 0;
    }
    let mut best = 0;
    let mut previous = vec![0; second.len()];
    let mut current = vec![0; second.len()];
    for first_word in first {
        for (index, second_word) in second.iter().enumerate() {
            current[index] = if first_word == second_word {
                if index == 0 {
                    1
                } else {
                    previous[index - 1] + 1
                }
            } else {
                0
            };
            best = best.max(current[index]);
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMPT: &str = "John Doe speaking. Professional, culinary register: French pastry, \
        sourdough baking, fermentation science, restaurant kitchen workflows. Speech is in \
        English or French; transcribe in the spoken language.";

    #[test]
    fn empty_and_short_responses_never_echo() {
        assert!(!is_prompt_echo("", PROMPT));
        assert!(!is_prompt_echo("John.", PROMPT));
        assert!(!is_prompt_echo("hello world this is a test", ""));
    }

    #[test]
    fn full_and_partial_prompt_echoes_are_detected() {
        assert!(is_prompt_echo(PROMPT, PROMPT));
        assert!(is_prompt_echo(
            "JOHN DOE SPEAKING — professional / culinary register",
            PROMPT
        ));
        assert!(is_prompt_echo(
            "okay um John Doe speaking professional culinary register French pastry sourdough baking right",
            PROMPT
        ));
    }

    #[test]
    fn real_speech_is_not_flagged() {
        assert!(!is_prompt_echo(
            "I am working on the sourdough recipe for a French pastry tonight",
            PROMPT
        ));
    }

    #[test]
    fn longest_run_is_contiguous() {
        assert_eq!(
            longest_common_word_run(
                &["the", "quick", "brown", "fox"],
                &["jumps", "over", "the", "quick", "brown", "dog"]
            ),
            3
        );
    }
}
