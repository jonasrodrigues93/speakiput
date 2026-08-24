// Adapted from whisrs src/llm.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

//! Optional LLM post-processing providers and output sanitation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SYSTEM_PROMPT: &str = "You are a text editing assistant. Apply the instruction to the text and return ONLY the resulting text. Your reply is inserted directly into the focused application: no preamble, explanation, commentary, surrounding quotes, markdown formatting or code fences.";

const LINE_BREAKS: [char; 7] = [
    '\n', '\r', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub model_id: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("LLM provider configuration is invalid: {0}")]
    InvalidConfiguration(String),
    #[error("LLM request failed: {0}")]
    Request(String),
    #[error("LLM provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("LLM provider returned no usable text")]
    EmptyResponse,
}

#[async_trait]
pub trait PostProcessor: Send + Sync {
    async fn process(&self, text: &str, instruction: &str) -> Result<String, LlmError>;
}

pub struct OpenAiCompatibleProvider {
    config: ProviderConfig,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: ProviderConfig) -> Result<Self, LlmError> {
        let endpoint = reqwest::Url::parse(&config.endpoint)
            .map_err(|error| LlmError::InvalidConfiguration(error.to_string()))?;
        if config.model_id.trim().is_empty() {
            return Err(LlmError::InvalidConfiguration(
                "model_id must not be empty".into(),
            ));
        }
        let loopback = endpoint
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]"));
        if !loopback && config.api_key.as_deref().is_none_or(str::is_empty) {
            return Err(LlmError::InvalidConfiguration(
                "a credential is required for non-loopback endpoints".into(),
            ));
        }
        Ok(Self {
            config,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl PostProcessor for OpenAiCompatibleProvider {
    async fn process(&self, text: &str, instruction: &str) -> Result<String, LlmError> {
        let request = ChatRequest {
            model: self.config.model_id.clone(),
            messages: rewrite_messages(text, instruction),
            temperature: 0.3,
        };
        let api_key = self.config.api_key.as_deref().unwrap_or("local");
        let response = self
            .client
            .post(&self.config.endpoint)
            .bearer_auth(api_key)
            .json(&request)
            .send()
            .await
            .map_err(|error| LlmError::Request(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::Http {
                status: status.as_u16(),
                body,
            });
        }
        let response = response
            .json::<ChatResponse>()
            .await
            .map_err(|error| LlmError::Request(error.to_string()))?;
        let cleaned = response
            .choices
            .first()
            .map(|choice| clean_llm_output(&choice.message.content))
            .unwrap_or_default();
        if cleaned.is_empty() {
            return Err(LlmError::EmptyResponse);
        }
        Ok(cleaned)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ChatResponseMessage {
    content: String,
}

fn rewrite_messages(text: &str, instruction: &str) -> Vec<ChatMessage> {
    let examples = [
        (
            "what is the command to install steam on arch linux",
            "Treat the following text as a request and output only what is asked.",
            "sudo pacman -S steam",
        ),
        (
            "Where is the train station?",
            "Translate the following text into German. Return only the translation.",
            "Wo ist der Bahnhof?",
        ),
        (
            "wht is teh comand to instal steam",
            "Fix the spelling and grammar. Return only the corrected text.",
            "What is the command to install steam",
        ),
    ];
    let mut messages = vec![ChatMessage {
        role: "system",
        content: SYSTEM_PROMPT.into(),
    }];
    for (example_text, example_instruction, reply) in examples {
        messages.push(ChatMessage {
            role: "user",
            content: frame_request(example_text, example_instruction),
        });
        messages.push(ChatMessage {
            role: "assistant",
            content: reply.into(),
        });
    }
    messages.push(ChatMessage {
        role: "user",
        content: frame_request(text, instruction),
    });
    messages
}

fn frame_request(text: &str, instruction: &str) -> String {
    format!("Text:\n{text}\n\nInstruction: {instruction}")
}

#[must_use]
pub fn clean_llm_output(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    strip_wrapping_code_fence(&normalized).trim().to_owned()
}

#[must_use]
pub fn contains_line_break(text: &str) -> bool {
    text.contains(LINE_BREAKS)
}

fn strip_wrapping_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return text;
    };
    let Some((info, body)) = rest.split_once('\n') else {
        return text;
    };
    if info.contains("```") || !is_fence_language_tag(info) {
        return text;
    }
    let Some(body) = body.trim_end().strip_suffix("```") else {
        return text;
    };
    if body.contains("```") { text } else { body }
}

fn is_fence_language_tag(info: &str) -> bool {
    let info = info.trim();
    info.is_empty()
        || info.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '+' | '-' | '_' | '#' | '.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaning_strips_padding_line_endings_and_wrapping_fences() {
        assert_eq!(clean_llm_output("  hello\r\n"), "hello");
        assert_eq!(
            clean_llm_output("```bash\nsudo pacman -S steam\n```"),
            "sudo pacman -S steam"
        );
        assert_eq!(
            clean_llm_output("```python\ndef f():\n    return 1\n```"),
            "def f():\n    return 1"
        );
    }

    #[test]
    fn cleaning_does_not_destroy_ambiguous_fences() {
        let multiple = "```sh\nfirst\n```\nthen\n```sh\nsecond\n```";
        assert_eq!(clean_llm_output(multiple), multiple);
        let content_on_fence = "```bash echo hi\necho bye\n```";
        assert_eq!(clean_llm_output(content_on_fence), content_on_fence);
    }

    #[test]
    fn every_dangerous_line_break_is_detected() {
        for line_break in LINE_BREAKS {
            assert!(contains_line_break(&format!("one{line_break}two")));
        }
    }

    #[test]
    fn messages_end_with_the_actual_request() {
        let messages = rewrite_messages("hello", "punctuate");
        assert_eq!(messages.first().unwrap().role, "system");
        assert_eq!(
            messages.last().unwrap().content,
            frame_request("hello", "punctuate")
        );
    }

    #[test]
    fn loopback_provider_accepts_no_key() {
        assert!(
            OpenAiCompatibleProvider::new(ProviderConfig {
                endpoint: "http://127.0.0.1:1234/v1/chat/completions".into(),
                model_id: "local".into(),
                api_key: None,
            })
            .is_ok()
        );
    }
}
