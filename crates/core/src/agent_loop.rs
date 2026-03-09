use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::handler::Handler;
use crate::llm::ToolClient;

/// Mirrors Python's StepOutcome dataclass.
#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub data: Option<Value>,
    pub next_prompt: Option<String>,
    pub should_exit: bool,
}

impl StepOutcome {
    pub fn new(data: Option<Value>, next_prompt: Option<String>, should_exit: bool) -> Self {
        StepOutcome {
            data,
            next_prompt,
            should_exit,
        }
    }

    pub fn done(data: Option<Value>) -> Self {
        StepOutcome {
            data,
            next_prompt: None,
            should_exit: false,
        }
    }

    pub fn exit(data: Option<Value>) -> Self {
        StepOutcome {
            data,
            next_prompt: None,
            should_exit: true,
        }
    }

    pub fn next(data: Option<Value>, prompt: impl Into<String>) -> Self {
        StepOutcome {
            data,
            next_prompt: Some(prompt.into()),
            should_exit: false,
        }
    }
}

/// Final result from the agent loop.
#[derive(Debug, Clone)]
pub enum AgentResult {
    CurrentTaskDone(Option<Value>),
    Exited(Option<Value>),
    MaxTurnsExceeded,
    Error(String),
}

/// Format JSON data for display (pretty-print with script expansion).
pub fn get_pretty_json(data: &Value) -> String {
    let mut data = data.clone();
    if let Some(obj) = data.as_object_mut() {
        if let Some(script) = obj.get("script").and_then(|s| s.as_str()) {
            let expanded = script.replace("; ", ";\n  ");
            obj.insert("script".to_string(), Value::String(expanded));
        }
    }
    serde_json::to_string_pretty(&data)
        .unwrap_or_default()
        .replace("\\n", "\n")
}

/// Convert a Value to display string for tool results
pub fn value_to_display(data: &Value) -> String {
    match data {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(data).unwrap_or_default(),
    }
}

/// JSON serializer that converts sets (arrays) to lists - just pass through for Rust
pub fn json_default_serialize(data: &Value) -> String {
    serde_json::to_string(data).unwrap_or_else(|_| data.to_string())
}

/// Main agent runner loop.
/// Mirrors Python's `agent_runner_loop` generator, adapted to async + channel streaming.
///
/// - `tx`: channel to send streaming text chunks to the caller
/// - `tools_schema`: JSON array of tool definitions
/// - `verbose`: if true, stream raw LLM output; if false, only send tool info
#[allow(clippy::too_many_arguments)]
pub async fn agent_runner_loop(
    client: &mut ToolClient,
    system_prompt: &str,
    user_input: &str,
    handler: &mut dyn Handler,
    tools_schema: &[Value],
    max_turns: usize,
    verbose: bool,
    tx: &UnboundedSender<String>,
) -> AgentResult {
    // Build initial message list
    let mut messages: Vec<serde_json::Map<String, Value>> = vec![
        {
            let mut m = serde_json::Map::new();
            m.insert("role".to_string(), Value::String("system".to_string()));
            m.insert(
                "content".to_string(),
                Value::String(system_prompt.to_string()),
            );
            m
        },
        {
            let mut m = serde_json::Map::new();
            m.insert("role".to_string(), Value::String("user".to_string()));
            m.insert("content".to_string(), Value::String(user_input.to_string()));
            m
        },
    ];

    for turn in 0..max_turns {
        let turn_num = turn + 1;
        let _ = tx.send(format!("**LLM Running (Turn {}) ...**\n\n", turn_num));

        // Reset tool cache every 10 turns
        if turn_num % 10 == 0 {
            client.last_tools = String::new();
        }

        // Call LLM
        let llm_tx = if verbose { Some(tx) } else { None };
        let response = match client.chat(&messages, tools_schema, llm_tx).await {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(format!("**LLM Error: {}**\n", e));
                return AgentResult::Error(e.to_string());
            }
        };

        if verbose {
            let _ = tx.send("\n\n".to_string());
        } else {
            // Non-verbose: just send content summary
            let _ = tx.send(response.content.clone());
        }

        // Determine which tool was called
        let (tool_name, args) = if response.tool_calls.is_empty() {
            ("no_tool".to_string(), Value::Object(serde_json::Map::new()))
        } else {
            let tc = &response.tool_calls[0];
            let args_val = serde_json::from_str::<Value>(&tc.function.arguments)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
            (tc.function.name.clone(), args_val)
        };

        // Show tool invocation
        if tool_name != "no_tool" {
            let showarg = get_pretty_json(&args);
            let showarg = if !verbose && showarg.len() > 200 {
                format!("{} ...", &showarg[..200])
            } else {
                showarg
            };
            let _ = tx.send(format!(
                "🛠️ **正在调用工具:** `{}`  📥**参数:**\n````text\n{}\n````\n",
                tool_name, showarg
            ));
        }

        handler.set_current_turn(turn_num);

        if verbose {
            let _ = tx.send("`````\n".to_string());
        }

        let outcome = handler.dispatch(&tool_name, &args, &response, tx).await;

        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => {
                let _ = tx.send(format!("**Handler error: {}**\n", e));
                StepOutcome::next(None, format!("Handler error: {}", e))
            }
        };

        if verbose {
            let _ = tx.send("`````\n".to_string());
        }

        // Handle outcome
        if outcome.next_prompt.is_none() {
            return AgentResult::CurrentTaskDone(outcome.data);
        }
        if outcome.should_exit {
            return AgentResult::Exited(outcome.data);
        }

        let next_prompt_str = outcome.next_prompt.as_deref().unwrap_or("");
        if next_prompt_str.starts_with("未知工具") {
            client.last_tools = String::new();
        }

        // Build next message content
        let mut next_content = String::new();
        if let Some(data) = &outcome.data {
            let data_str = match data {
                Value::String(s) => s.clone(),
                _ => serde_json::to_string(data).unwrap_or_default(),
            };
            next_content.push_str(&format!("<tool_result>\n{}\n</tool_result>\n\n", data_str));
        }
        next_content.push_str(next_prompt_str);

        // Apply next_prompt_patcher
        let next_content = handler.next_prompt_patcher(&next_content, &outcome, turn_num);

        debug!(
            "Turn {} outcome, next prompt len: {}",
            turn_num,
            next_content.len()
        );

        messages = vec![{
            let mut m = serde_json::Map::new();
            m.insert("role".to_string(), Value::String("user".to_string()));
            m.insert("content".to_string(), Value::String(next_content));
            m
        }];
    }

    AgentResult::MaxTurnsExceeded
}
