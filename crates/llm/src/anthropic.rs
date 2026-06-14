use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{Message, Role};

pub struct AnthropicClient {
    client: Client,
    api_key: String,
    model: String,
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ApiMessage>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct Response {
    content: Vec<ContentBlock>,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build HTTP client");
        Self {
            client,
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub async fn complete(&self, messages: Vec<Message>) -> anyhow::Result<String> {
        let system = messages
            .iter()
            .find(|m| m.role == Role::System)
            .map(|m| m.content.clone());

        let api_messages: Vec<ApiMessage> = messages
            .into_iter()
            .filter(|m| m.role != Role::System)
            .map(|m| ApiMessage {
                role: m.role.to_string(),
                content: m.content,
            })
            .collect();

        let request = Request {
            model: &self.model,
            max_tokens: 1024,
            system,
            messages: api_messages,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&request)
            .send()
            .await
            .context("failed to send request to Anthropic API")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Anthropic response body")?;

        if !status.is_success() {
            bail!("Anthropic API returned {status}: {body}");
        }

        let resp: Response = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse Anthropic response: {body}"))?;

        resp.content
            .into_iter()
            .find(|b| b.kind == "text")
            .and_then(|b| b.text)
            .context("Anthropic response contained no text content")
    }
}
