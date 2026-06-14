use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::Message;

pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: Vec<ApiMessage>,
    stream: bool,
    /// Disable extended thinking on models that support it (e.g. qwen3).
    /// When thinking is on, these models emit content only in the `thinking`
    /// field and leave `content` empty, producing no usable output.
    think: bool,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct Response {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    name: String,
}

/// Remove `<think>…</think>` reasoning blocks emitted by thinking models
/// (e.g. qwen3, deepseek-r1) and return the remaining text trimmed.
fn strip_thinking(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("</think>") {
            Some(end) => &rest[start + end + "</think>".len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Return the names of all models currently available in the Ollama instance.
pub async fn list_models(base_url: &str) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let resp = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client")
        .get(&url)
        .send()
        .await
        .context("failed to reach Ollama")?;
    let tags: TagsResponse = resp.json().await.context("failed to parse Ollama model list")?;
    Ok(tags.models.into_iter().map(|m| m.name).collect())
}

impl OllamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build HTTP client");
        Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
        }
    }

    pub async fn complete(&self, messages: Vec<Message>) -> anyhow::Result<String> {
        let api_messages: Vec<ApiMessage> = messages
            .into_iter()
            .map(|m| ApiMessage {
                role: m.role.to_string(),
                content: m.content,
            })
            .collect();

        let request = Request {
            model: &self.model,
            messages: api_messages,
            stream: false,
            think: false,
        };

        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("failed to send request to Ollama")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Ollama response body")?;

        if !status.is_success() {
            bail!("Ollama returned {status}: {body}");
        }

        let resp: Response = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse Ollama response: {body}"))?;

        Ok(strip_thinking(&resp.message.content))
    }
}
