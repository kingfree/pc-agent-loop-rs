pub mod agent_loop;
pub mod handler;
pub mod llm;
pub mod tools;
pub mod webdriver;

pub use agent_loop::{agent_runner_loop, AgentResult, StepOutcome};
pub use handler::GenericAgentHandler;
pub use llm::{AppConfig, ToolClient};

/// High-level async API for embedding the agent in other applications.
/// This is what Android/iOS bindings use.
pub struct AgentSession {
    config: AppConfig,
    work_dir: std::path::PathBuf,
    history: Vec<String>,
    key_info: String,
}

impl AgentSession {
    pub fn new(config_json: &str, work_dir: &str) -> anyhow::Result<Self> {
        let config: AppConfig = serde_json::from_str(config_json)
            .map_err(|e| anyhow::anyhow!("Failed to parse config JSON: {}", e))?;
        Ok(AgentSession {
            config,
            work_dir: std::path::PathBuf::from(work_dir),
            history: Vec::new(),
            key_info: String::new(),
        })
    }

    /// Run one agent turn; calls `callback` with each streamed chunk.
    /// Returns the final result as a JSON string.
    pub async fn run_task(
        &mut self,
        task: &str,
        max_turns: usize,
        callback: impl Fn(&str) + Send + 'static,
    ) -> anyhow::Result<String> {
        use tokio::sync::mpsc;

        let mut client = ToolClient::new(self.config.clone())?;
        let work_dir_str = self.work_dir.to_string_lossy().to_string();
        let mut handler = GenericAgentHandler::new(task, &work_dir_str);

        // Restore key_info from previous session state
        handler.key_info = self.key_info.clone();
        handler.history_info = self.history.clone();

        // Build system prompt
        let system_prompt = format!(
            "你是一个强大的AI代理，能够通过工具完成各种计算机任务。\n\n## 当前任务\n{}\n\n## 工作目录\n{}\n",
            task, work_dir_str
        );

        let tools: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "name": "code_run",
                "description": "执行Python或Bash代码",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "language": { "type": "string", "enum": ["python", "bash"] },
                        "code": { "type": "string" },
                        "timeout": { "type": "integer" }
                    },
                    "required": ["language", "code"]
                }
            }),
            serde_json::json!({
                "name": "file_read",
                "description": "读取文件内容",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }
            }),
            serde_json::json!({
                "name": "file_write",
                "description": "写入文件",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" },
                        "mode": { "type": "string", "enum": ["overwrite", "append", "prepend"] }
                    },
                    "required": ["path", "content"]
                }
            }),
        ];

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Spawn output streaming task
        let stream_task = tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                callback(&chunk);
            }
        });

        let result = agent_runner_loop(
            &mut client,
            &system_prompt,
            task,
            &mut handler,
            &tools,
            max_turns,
            false,
            &tx,
        )
        .await;

        // Persist state for next call
        self.key_info = handler.key_info.clone();
        self.history = handler.history_info.clone();

        drop(tx);
        let _ = stream_task.await;

        let result_json = match result {
            AgentResult::CurrentTaskDone(data) => serde_json::json!({
                "status": "done",
                "data": data
            }),
            AgentResult::Exited(data) => serde_json::json!({
                "status": "exited",
                "data": data
            }),
            AgentResult::MaxTurnsExceeded => serde_json::json!({
                "status": "max_turns_exceeded"
            }),
            AgentResult::Error(e) => serde_json::json!({
                "status": "error",
                "error": e
            }),
        };

        Ok(serde_json::to_string(&result_json)?)
    }
}
