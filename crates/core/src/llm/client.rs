use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

use crate::llm::session::{ClaudeSession, LLMSession};
use crate::llm::types::{AppConfig, MockResponse, MockToolCall};

/// Backend selection
pub enum LLMBackend {
    OpenAI(LLMSession),
    Claude(ClaudeSession),
}

/// ToolClient: wraps LLM backends, builds text-protocol prompts, parses tool calls.
/// Mirrors Python's ToolClient class.
pub struct ToolClient {
    pub backend: LLMBackend,
    pub last_tools: String,
    pub total_cd_tokens: usize,
    pub auto_save_tokens: bool,
}

impl ToolClient {
    pub fn new(config: AppConfig) -> Result<Self> {
        let auto_save_tokens = true;
        let backend = if config.claude_config.is_some() {
            LLMBackend::Claude(ClaudeSession::new(&config)?)
        } else if config.oai_config.is_some() {
            LLMBackend::OpenAI(LLMSession::new(config)?)
        } else {
            return Err(anyhow!("No LLM backend configured in mykey.json"));
        };
        Ok(ToolClient {
            backend,
            last_tools: String::new(),
            total_cd_tokens: 0,
            auto_save_tokens,
        })
    }

    /// Build the text-protocol prompt from messages and tool schemas.
    /// Mirrors Python's `_build_protocol_prompt`.
    pub fn build_protocol_prompt(
        &mut self,
        messages: &[serde_json::Map<String, Value>],
        tools: &[Value],
    ) -> String {
        let system_content: String = messages
            .iter()
            .find(|m| {
                m.get("role")
                    .and_then(|r| r.as_str())
                    .map(|r| r.to_lowercase())
                    == Some("system".to_string())
            })
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let history_msgs: Vec<&serde_json::Map<String, Value>> = messages
            .iter()
            .filter(|m| {
                m.get("role")
                    .and_then(|r| r.as_str())
                    .map(|r| r.to_lowercase())
                    != Some("system".to_string())
            })
            .collect();

        let tool_instruction = if !tools.is_empty() {
            let tools_json = serde_json::to_string(tools).unwrap_or_default();
            let instruction = if self.auto_save_tokens && self.last_tools == tools_json {
                "\n### 工具库状态：持续有效，**可正常调用**。调用协议沿用。\n".to_string()
            } else {
                self.total_cd_tokens = 0;
                format!(
                    r#"
### 交互协议 (必须严格遵守，持续有效)
请按照以下步骤思考并行动，标签之间需要回车换行：
1. **思考**: 在 `<thinking>` 标签中先进行思考，分析现状和策略。
2. **总结**: 在 `<summary>` 中输出*极为简短*的高度概括的单行（<30字）物理快照。
3. **行动**: 如需调用工具，请在回复正文之后输出一个 **<tool_use>块**，然后结束。
   格式: ```<tool_use>\n{{"name": "工具名", "arguments": {{参数}}}}\n</tool_use>\n```

### 可用工具库（已挂载，持续有效）
{tools_json}
"#
                )
            };
            self.last_tools = tools_json;
            instruction
        } else {
            String::new()
        };

        let mut prompt = String::new();
        if !system_content.is_empty() {
            prompt.push_str(&format!("=== SYSTEM ===\n{}\n", system_content));
        }
        prompt.push_str(&tool_instruction);
        prompt.push_str("\n\n");

        for m in &history_msgs {
            let role_raw = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let role = if role_raw == "user" {
                "USER"
            } else {
                "ASSISTANT"
            };
            let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
            prompt.push_str(&format!("=== {} ===\n{}\n\n", role, content));
            self.total_cd_tokens += content.len();
        }

        if self.total_cd_tokens > 6000 {
            self.last_tools = String::new();
        }

        prompt.push_str("=== ASSISTANT ===\n");
        prompt
    }

    /// Parse the mixed response text into a MockResponse.
    /// Mirrors Python's `_parse_mixed_response`.
    pub fn parse_mixed_response(&mut self, text: &str) -> MockResponse {
        let mut remaining = text.to_string();

        // Extract <thinking>
        let think_re = Regex::new(r"(?s)<thinking>(.*?)</thinking>").unwrap();
        let thinking = think_re
            .captures(&remaining)
            .map(|c| c[1].trim().to_string())
            .unwrap_or_default();
        remaining = think_re.replace_all(&remaining, "").to_string();

        let mut tool_calls: Vec<MockToolCall> = Vec::new();
        let mut json_strs: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // Try complete <tool_use>...</tool_use> blocks
        let tool_all_re = Regex::new(r"(?s)<tool_use>(.{15,}?)</tool_use>").unwrap();
        let tool_all: Vec<String> = tool_all_re
            .captures_iter(&remaining)
            .map(|c| c[1].trim().to_string())
            .collect();

        if !tool_all.is_empty() {
            for s in &tool_all {
                if s.starts_with('{') && s.ends_with('}') {
                    json_strs.push(s.clone());
                }
            }
            remaining = tool_all_re.replace_all(&remaining, "").to_string();
        } else if remaining.contains("<tool_use>") {
            // Partial/unclosed tag
            let parts: Vec<&str> = remaining.splitn(2, "<tool_use>").collect();
            if parts.len() == 2 {
                let weaktoolstr = parts[1].trim();
                let json_str = if weaktoolstr.ends_with('}') {
                    weaktoolstr.to_string()
                } else {
                    String::new()
                };
                if !json_str.is_empty() {
                    json_strs.push(json_str.clone());
                    remaining = remaining.replace(&format!("<tool_use>{}", weaktoolstr), "");
                }
            }
        } else if remaining.contains("\"name\":") && remaining.contains("\"arguments\":") {
            // Fallback: find JSON object with name+arguments
            let json_re = Regex::new(r#"(?s)(\{.*?"name":.*?\})"#).unwrap();
            if let Some(cap) = json_re.captures(&remaining) {
                let json_str = cap[1].trim().to_string();
                remaining = remaining.replace(&cap[1], "").trim().to_string();
                json_strs.push(json_str);
            }
        }

        for json_str in &json_strs {
            match try_parse_json(json_str) {
                Ok(data) => {
                    let func_name = data
                        .get("name")
                        .or_else(|| data.get("function"))
                        .or_else(|| data.get("tool"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let args = data
                        .get("arguments")
                        .or_else(|| data.get("args"))
                        .or_else(|| data.get("params"))
                        .or_else(|| data.get("parameters"))
                        .cloned()
                        .unwrap_or_else(|| data.clone());

                    if let Some(name) = func_name {
                        tool_calls.push(MockToolCall::new(name, args));
                    }
                }
                Err(e) => {
                    warn!("Failed to parse tool_use JSON: {}", e);
                    let preview = if json_str.len() > 200 {
                        &json_str[..200]
                    } else {
                        json_str.as_str()
                    };
                    errors.push(format!("Failed to parse tool_use JSON: {}", preview));
                    self.last_tools = String::new();
                }
            }
        }

        // If parse errors but no successful tool_calls, emit bad_json tool
        if tool_calls.is_empty() && !errors.is_empty() {
            for e in &errors {
                tool_calls.push(MockToolCall::new(
                    "bad_json",
                    serde_json::json!({ "msg": e }),
                ));
            }
        }

        let content = remaining.trim().to_string();
        // Only keep last tool call (matching Python's `tool_calls[-1:]`)
        let last_tool = if tool_calls.is_empty() {
            vec![]
        } else {
            vec![tool_calls.pop().unwrap()]
        };

        MockResponse::new(thinking, content, last_tool, text.to_string())
    }

    /// Main chat method: builds prompt, calls LLM, parses response.
    /// Returns (MockResponse, Vec<String>) where Vec<String> are streamed text chunks.
    pub async fn chat(
        &mut self,
        messages: &[serde_json::Map<String, Value>],
        tools: &[Value],
        tx: Option<&UnboundedSender<String>>,
    ) -> Result<MockResponse> {
        let prompt = self.build_protocol_prompt(messages, tools);
        debug!("Prompt length: {} chars", prompt.len());

        let raw_text = match &self.backend {
            LLMBackend::OpenAI(session) => session.stream_completion(&prompt, tx, 3).await?,
            LLMBackend::Claude(session) => session.stream_completion(&prompt, tx, 3).await?,
        };

        Ok(self.parse_mixed_response(&raw_text))
    }
}

/// Try to parse JSON with some leniency (handles trailing commas, etc.)
fn try_parse_json(s: &str) -> Result<Value> {
    // First try direct parse
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Ok(v);
    }
    // Try stripping markdown code fences
    let stripped = s
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str::<Value>(stripped).map_err(|e| anyhow!("JSON parse error: {}", e))
}
