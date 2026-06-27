use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::Message;

pub struct OllamaClient {
    client: Client,
    base_url: String,
    model: String,
    keep_alive: Option<String>,
    num_ctx: u32,
}

#[derive(Serialize)]
struct Options {
    num_ctx: u32,
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
    /// How long Ollama keeps the model loaded after this request. Accepts
    /// Ollama duration strings ("5m", "1h") or "-1" for indefinite. When
    /// None the server default (5 minutes) applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_alive: Option<&'a str>,
    options: Options,
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

/// Request body used to load/unload a model without generating text.
/// `keep_alive: 0` tells Ollama to evict the model from memory immediately.
#[derive(Serialize)]
struct KeepAliveRequest<'a> {
    model: &'a str,
    keep_alive: u32,
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
    let tags: TagsResponse = resp
        .json()
        .await
        .context("failed to parse Ollama model list")?;
    Ok(tags.models.into_iter().map(|m| m.name).collect())
}

/// Return the names of the models currently loaded in memory (the `/api/ps`
/// endpoint). These are the models that are "running" and consuming RAM/VRAM.
pub async fn running_models(base_url: &str) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/api/ps", base_url.trim_end_matches('/'));
    let resp = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client")
        .get(&url)
        .send()
        .await
        .context("failed to reach Ollama")?;
    let ps: TagsResponse = resp
        .json()
        .await
        .context("failed to parse Ollama running model list")?;
    Ok(ps.models.into_iter().map(|m| m.name).collect())
}

impl OllamaClient {
    pub fn model_name(&self) -> &str {
        &self.model
    }

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
            keep_alive: None,
            num_ctx: 8192,
        }
    }

    /// Set the Ollama `keep_alive` duration for all requests made by this client.
    /// Accepts Ollama duration strings like "10m", "1h", or "-1" (indefinite).
    /// When not set, Ollama's server default (5 minutes) applies.
    pub fn with_keep_alive(mut self, duration: impl Into<String>) -> Self {
        self.keep_alive = Some(duration.into());
        self
    }

    /// Override the context window size sent to Ollama (`options.num_ctx`).
    /// Default is 8192. Use this to match the model's native context size.
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = num_ctx;
        self
    }

    /// Whether this client's model is currently loaded in Ollama's memory.
    pub async fn is_loaded(&self) -> anyhow::Result<bool> {
        let running = running_models(&self.base_url).await?;
        Ok(running.iter().any(|m| m == &self.model))
    }

    /// Evict this client's model from Ollama's memory by issuing an empty
    /// request with `keep_alive: 0`.
    pub async fn unload(&self) -> anyhow::Result<()> {
        let url = format!("{}/api/generate", self.base_url.trim_end_matches('/'));
        let request = KeepAliveRequest {
            model: &self.model,
            keep_alive: 0,
        };
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("failed to send unload request to Ollama")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("Ollama returned {status} on unload: {body}");
        }
        Ok(())
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
            keep_alive: self.keep_alive.as_deref(),
            options: Options { num_ctx: self.num_ctx },
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
