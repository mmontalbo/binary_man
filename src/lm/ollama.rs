//! Ollama HTTP API plugin with persistent conversation history.
//!
//! Maintains message history across prompts for multi-turn context,
//! similar to ClaudeCodePlugin but using Ollama's local HTTP API.
//!
//! Uses `POST /api/chat` with `stream: false` for synchronous responses.

use super::LmPlugin;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default Ollama API base URL.
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

/// Reserve this many tokens for the next prompt + response (rough estimate).
/// We truncate history when estimated usage exceeds num_ctx minus this reserve.
const CONTEXT_RESERVE_TOKENS: usize = 8192;

/// Rough chars-per-token estimate for English/code mixed content.
const CHARS_PER_TOKEN: usize = 4;

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

/// Request body for Ollama /api/chat.
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    options: ChatOptions,
}

/// Model parameters for the chat request.
#[derive(Serialize)]
struct ChatOptions {
    /// Context window size in tokens.
    num_ctx: u32,
    /// Temperature for sampling.
    temperature: f32,
}

/// Response from Ollama /api/chat (non-streaming).
#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

/// Ollama plugin with persistent conversation state.
///
/// Maintains a rolling message history and communicates with
/// the local Ollama server via its HTTP API.
pub struct OllamaPlugin {
    model: String,
    base_url: String,
    messages: Vec<ChatMessage>,
}

impl OllamaPlugin {
    /// Create a new Ollama plugin for the specified model.
    ///
    /// # Arguments
    /// * `model` - Ollama model tag, e.g. "devstral-small-2:24b"
    pub fn new(model: &str) -> Self {
        let base_url = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self {
            model: model.to_string(),
            base_url,
            messages: Vec::new(),
        }
    }

    /// Drop oldest message pairs until estimated token usage fits within budget.
    fn truncate_history(&mut self, max_tokens: usize, new_prompt_len: usize) {
        let estimate_tokens =
            |msgs: &[ChatMessage], extra: usize| -> usize {
                let char_total: usize = msgs.iter().map(|m| m.content.len()).sum();
                (char_total + extra) / CHARS_PER_TOKEN
            };

        while estimate_tokens(&self.messages, new_prompt_len) > max_tokens
            && self.messages.len() >= 2
        {
            // Drop oldest pair (user, assistant)
            self.messages.drain(..2);
        }
    }
}

impl LmPlugin for OllamaPlugin {
    fn init(&mut self) -> Result<()> {
        // Verify connectivity by hitting the version endpoint
        let url = format!("{}/api/version", self.base_url);
        ureq::get(&url)
            .call()
            .map_err(|e| anyhow!("Cannot reach Ollama at {}: {}", self.base_url, e))?;
        eprintln!("Ollama: connected to {}, model={}", self.base_url, self.model);
        Ok(())
    }

    fn prompt(&mut self, prompt: &str, timeout: Duration) -> Result<String> {
        // Truncate old messages if approaching the context window limit.
        // Keep dropping the oldest pair (user+assistant) until we're under budget.
        let max_history_tokens = 32768_usize.saturating_sub(CONTEXT_RESERVE_TOKENS);
        self.truncate_history(max_history_tokens, prompt.len());

        // Append user message to history
        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        });

        let url = format!("{}/api/chat", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages: &self.messages,
            stream: false,
            options: ChatOptions {
                num_ctx: 32768,
                temperature: 0.3,
            },
        };

        let timeout_secs = timeout.as_secs();
        let config = ureq::config::Config::builder()
            .timeout_global(Some(timeout))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut resp = agent
            .post(&url)
            .send_json(&body)
            .map_err(|e| {
                // On timeout/error, pop the user message so retry is clean
                self.messages.pop();
                anyhow!("Ollama chat request failed (timeout={}s): {}", timeout_secs, e)
            })?;

        let chat_resp: ChatResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| {
                self.messages.pop();
                anyhow!("Failed to parse Ollama response: {}", e)
            })?;

        let content = chat_resp.message.content.clone();

        // Append assistant response to history for multi-turn context
        self.messages.push(chat_resp.message);

        Ok(content)
    }

    fn reset(&mut self) -> Result<()> {
        self.messages.clear();
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        self.messages.clear();
        Ok(())
    }

    fn is_stateful(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_default_url() {
        let plugin = OllamaPlugin::new("qwen3-coder:30b");
        assert_eq!(plugin.model, "qwen3-coder:30b");
        assert!(plugin.messages.is_empty());
    }

    #[test]
    fn test_reset_clears_history() {
        let mut plugin = OllamaPlugin::new("test");
        plugin.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        });
        plugin.reset().unwrap();
        assert!(plugin.messages.is_empty());
    }

    #[test]
    fn test_is_stateful() {
        let plugin = OllamaPlugin::new("test");
        assert!(plugin.is_stateful());
    }
}
