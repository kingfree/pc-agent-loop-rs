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
        let oai = config
            .oai_config
            .as_ref()
            .ok_or_else(|| anyhow!("oai_config is required for LLMSession"))?;

        let mut client_builder = Client::builder().timeout(std::time::Duration::from_secs(120));

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

        let resp = self
            .client
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
                    let data = line.strip_prefix("data: ").unwrap_or(&line);
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
        let claude = config
            .claude_config
            .as_ref()
            .ok_or_else(|| anyhow!("claude_config is required for ClaudeSession"))?;

        let mut client_builder = Client::builder().timeout(std::time::Duration::from_secs(120));

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

        let resp = self
            .client
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
                    let data = line.strip_prefix("data: ").unwrap_or(&line);
                    if let Ok(json_val) = serde_json::from_str::<Value>(data) {
                        // Claude streaming: content_block_delta events
                        if json_val.get("type").and_then(|t| t.as_str())
                            == Some("content_block_delta")
                        {
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

/// Google Gemini API session
pub struct GeminiSession {
    pub api_key: String,
    pub model: String,
    pub api_base: String,
}

impl GeminiSession {
    pub fn new(api_key: String, model: String) -> Self {
        GeminiSession {
            api_key,
            model,
            api_base: "https://generativelanguage.googleapis.com/v1".to_string(),
        }
    }

    pub async fn stream_completion(
        &self,
        prompt: &str,
        tx: &UnboundedSender<String>,
    ) -> Result<String> {
        use futures::StreamExt;

        let url = format!(
            "{}/models/{}:streamGenerateContent?key={}",
            self.api_base, self.model, self.api_key
        );

        let body = serde_json::json!({
            "contents": [
                {
                    "role": "user",
                    "parts": [{"text": prompt}]
                }
            ],
            "generationConfig": {
                "maxOutputTokens": 4096,
                "temperature": 0.7
            }
        });

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Gemini API error {}: {}", status, text));
        }

        let mut stream = resp.bytes_stream();
        let mut full_text = String::new();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buffer.push_str(&chunk_str);

            // Gemini returns a JSON array streamed in chunks; parse complete objects.
            // Each chunk: {"candidates":[{"content":{"parts":[{"text":"..."}]}}]}
            let mut search_start = 0;
            while let Some(obj_start) = buffer[search_start..].find('{') {
                let abs_start = search_start + obj_start;
                let mut depth = 0i32;
                let mut end_pos = None;
                let bytes = &buffer.as_bytes()[abs_start..];
                let mut in_string = false;
                let mut escape_next = false;
                for (i, &b) in bytes.iter().enumerate() {
                    if escape_next {
                        escape_next = false;
                        continue;
                    }
                    if b == b'\\' && in_string {
                        escape_next = true;
                        continue;
                    }
                    if b == b'"' {
                        in_string = !in_string;
                        continue;
                    }
                    if in_string {
                        continue;
                    }
                    if b == b'{' {
                        depth += 1;
                    } else if b == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            end_pos = Some(abs_start + i + 1);
                            break;
                        }
                    }
                }

                if let Some(end) = end_pos {
                    let obj_str = &buffer[abs_start..end];
                    if let Ok(json_val) = serde_json::from_str::<Value>(obj_str) {
                        if let Some(text_part) = json_val
                            .get("candidates")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("content"))
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.get(0))
                            .and_then(|p| p.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            full_text.push_str(text_part);
                            let _ = tx.send(text_part.to_string());
                        }
                    }
                    search_start = end;
                } else {
                    break;
                }
            }

            if search_start > 0 {
                buffer = buffer[search_start..].to_string();
            }
        }

        debug!("Gemini response complete, {} chars", full_text.len());
        Ok(full_text)
    }
}
