use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Simulates Python's MockToolCall
#[derive(Debug, Clone)]
pub struct MockToolCall {
    pub function: MockFunction,
}

#[derive(Debug, Clone)]
pub struct MockFunction {
    pub name: String,
    pub arguments: String, // raw JSON string
}

impl MockToolCall {
    pub fn new(name: impl Into<String>, args: Value) -> Self {
        MockToolCall {
            function: MockFunction {
                name: name.into(),
                arguments: serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string()),
            },
        }
    }
}

/// Simulates Python's MockResponse
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub thinking: String,
    pub content: String,
    pub tool_calls: Vec<MockToolCall>,
    pub raw_text: String,
}

impl MockResponse {
    pub fn new(thinking: String, content: String, tool_calls: Vec<MockToolCall>, raw_text: String) -> Self {
        MockResponse { thinking, content, tool_calls, raw_text }
    }
}

/// Config loaded from mykey.json
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OaiConfig {
    pub apikey: String,
    pub apibase: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub oai_config: Option<OaiConfig>,
    pub claude_config: Option<ClaudeConfig>,
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClaudeConfig {
    pub apikey: String,
    pub model: String,
}

/// A tool schema entry as passed to the LLM
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
