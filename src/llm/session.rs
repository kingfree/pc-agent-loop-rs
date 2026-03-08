use anyhow::{anyhow, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

use crate::llm::types::AppConfig;

/// OpenAI-compatible streaming session
pub struct LLMSession {
    pub client: Client,
    #[allow(dead_code)]
    pub config: AppConfig,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl LLMSession {
    pub fn new(config: AppConfig) -> Result<Self> {
        let oai = config.oai_config.as_ref()
            .ok_or_else(|| anyhow!("oai_config is required for LLMSession"))?;

        let mut client_builder = Client::builder()
            .timeout(std::time::Duration::from_secs(120));

        if let Some(proxy_url) = &config.proxy {
            let proxy = reqwest::Proxy::all(proxy_url)?;
            client_builder = client_builder.proxy(proxy);
        }

        let client = client_builder.build()?;

        Ok(LLMSession {
            client,
            base_url: oai.apibase.trim_end_matches('/').to_string(),
            api_key: oai.apikey.clone(),
            model: oai.model.clone(),
            config,
        })
    }

    /// Stream completions via SSE, sending chunks through `tx`.
    /// Returns the full accumulated text.
    pub async fn stream_completion(
        &self,
        prompt: &str,
        tx: Option<&UnboundedSender<String>>,
        max_retries: u32,
    ) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "stream": true,
            "max_tokens": 4096,
            "temperature": 0.7,
        });

        let mut attempts = 0;
        loop {
            attempts += 1;
            let result = self.do_stream_request(&url, &body, tx).await;
            match result {
                Ok(text) => return Ok(text),
                Err(e) => {
                    warn!("LLM request attempt {} failed: {}", attempts, e);
                    if attempts >= max_retries {
                        return Err(e);
                    }
                    // Exponential backoff
                    let wait = std::time::Duration::from_secs(2u64.pow(attempts.min(5)));
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    async fn do_stream_request(
        &self,
        url: &str,
        body: &Value,
        tx: Option<&UnboundedSender<String>>,
    ) -> Result<String> {
        use futures::StreamExt;

        let resp = self.client
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("LLM API error {}: {}", status, text));
        }

        let mut stream = resp.bytes_stream();
        let mut full_text = String::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // Process complete SSE lines
            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.starts_with("data: ") {
                    let data = &line["data: ".len()..];
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(json_val) = serde_json::from_str::<Value>(data) {
                        if let Some(delta_content) = json_val
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            full_text.push_str(delta_content);
                            if let Some(sender) = tx {
                                let _ = sender.send(delta_content.to_string());
                            }
                        }
                    }
                }
            }
        }

        debug!("LLM response complete, {} chars", full_text.len());
        Ok(full_text)
    }
}

/// Claude (Anthropic) API session
pub struct ClaudeSession {
    pub client: Client,
    pub api_key: String,
    pub model: String,
}

impl ClaudeSession {
    pub fn new(config: &AppConfig) -> Result<Self> {
        let claude = config.claude_config.as_ref()
            .ok_or_else(|| anyhow!("claude_config is required for ClaudeSession"))?;

        let mut client_builder = Client::builder()
            .timeout(std::time::Duration::from_secs(120));

        if let Some(proxy_url) = &config.proxy {
            let proxy = reqwest::Proxy::all(proxy_url)?;
            client_builder = client_builder.proxy(proxy);
        }

        Ok(ClaudeSession {
            client: client_builder.build()?,
            api_key: claude.apikey.clone(),
            model: claude.model.clone(),
        })
    }

    pub async fn stream_completion(
        &self,
        prompt: &str,
        tx: Option<&UnboundedSender<String>>,
        max_retries: u32,
    ) -> Result<String> {
        let url = "https://api.anthropic.com/v1/messages";
        // Convert our text protocol prompt into Claude messages format
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "stream": true,
            "messages": [{"role": "user", "content": prompt}],
        });

        let mut attempts = 0;
        loop {
            attempts += 1;
            let result = self.do_stream_request(url, &body, tx).await;
            match result {
                Ok(text) => return Ok(text),
                Err(e) => {
                    warn!("Claude request attempt {} failed: {}", attempts, e);
                    if attempts >= max_retries {
                        return Err(e);
                    }
                    let wait = std::time::Duration::from_secs(2u64.pow(attempts.min(5)));
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    async fn do_stream_request(
        &self,
        url: &str,
        body: &Value,
        tx: Option<&UnboundedSender<String>>,
    ) -> Result<String> {
        use futures::StreamExt;

        let resp = self.client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Claude API error {}: {}", status, text));
        }

        let mut stream = resp.bytes_stream();
        let mut full_text = String::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.starts_with("data: ") {
                    let data = &line["data: ".len()..];
                    if let Ok(json_val) = serde_json::from_str::<Value>(data) {
                        // Claude streaming: content_block_delta events
                        if json_val.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                            if let Some(text_delta) = json_val
                                .get("delta")
                                .and_then(|d| d.get("text"))
                                .and_then(|t| t.as_str())
                            {
                                full_text.push_str(text_delta);
                                if let Some(sender) = tx {
                                    let _ = sender.send(text_delta.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(full_text)
    }
}
