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

impl OllamaClient {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
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

        Ok(resp.message.content)
    }
}
