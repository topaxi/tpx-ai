mod anthropic;
mod ollama;

pub use anthropic::AnthropicClient;
pub use ollama::{list_models as list_ollama_models, OllamaClient};

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::System => write!(f, "system"),
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
    }
}

pub enum LlmProvider {
    Anthropic(AnthropicClient),
    Ollama(OllamaClient),
}

impl LlmProvider {
    pub fn anthropic(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::Anthropic(AnthropicClient::new(api_key, model))
    }

    pub fn ollama(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self::Ollama(OllamaClient::new(base_url, model))
    }

    pub async fn complete(&self, messages: Vec<Message>) -> anyhow::Result<String> {
        match self {
            Self::Anthropic(c) => c.complete(messages).await,
            Self::Ollama(c) => c.complete(messages).await,
        }
    }
}
