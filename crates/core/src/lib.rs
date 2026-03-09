pub mod agent_loop;
pub mod handler;
pub mod llm;
pub mod tools;
pub mod webdriver;

pub use agent_loop::{agent_runner_loop, AgentResult, StepOutcome};
pub use handler::GenericAgentHandler;
pub use llm::{AppConfig, ToolClient};

/// Build the system prompt, matching Python's `get_system_prompt()`.
pub fn build_system_prompt(work_dir: &str) -> String {
    use std::path::Path;
    let assets_prompt = Path::new(work_dir).join("assets").join("sys_prompt.txt");
    let base = if assets_prompt.exists() {
        std::fs::read_to_string(&assets_prompt).unwrap_or_else(|_| builtin_sys_prompt())
    } else {
        builtin_sys_prompt()
    };
    let today = chrono::Local::now().format("%Y-%m-%d %a").to_string();
    let memory = load_global_memory_str(work_dir);
    format!("{}\nToday: {}\n{}", base, today, memory)
}

fn builtin_sys_prompt() -> String {
    concat!(
        "# Role: 物理级全能执行者\n",
        "你拥有文件读写、脚本执行、用户浏览器JS注入、系统级干预的物理操作权限。禁止推诿\u{201c}无法操作\u{201d}\u{2014}\u{2014}不空想，用工具探测。\n",
        "## 行动原则\n",
        "调用工具前在 <thinking> 内推演：当前阶段、上步结果是否符合预期、下步策略。\n",
        "- 探测优先：失败时先充分获取信息（日志/状态/上下文），关键信息存入工作记忆，再决定重试或换方案。不可逆操作先询问用户。\n",
        "- 失败升级：1次→读错误理解原因，2次→探测环境状态，3次→深度分析后换方案或问用户。禁止无新信息的重复操作。"
    ).to_string()
}

fn load_global_memory_str(work_dir: &str) -> String {
    use std::path::Path;
    let base = Path::new(work_dir);
    let insight = base.join("memory").join("global_mem_insight.txt");
    let structure = base.join("assets").join("insight_fixed_structure.txt");
    let mut result = String::new();
    if let Ok(text) = std::fs::read_to_string(&insight) {
        let struct_text = std::fs::read_to_string(&structure).unwrap_or_default();
        result.push_str("\n[Memory]\n");
        result.push_str(&format!("cwd = {} （用./引用）\n", base.join("temp").display()));
        if !struct_text.is_empty() {
            result.push_str(&struct_text);
            result.push('\n');
        }
        result.push_str("../memory/global_mem_insight.txt:\n");
        result.push_str(&text);
    }
    result
}

/// Full tools schema matching the Python original (assets/tools_schema.json).
pub fn full_tools_schema() -> Vec<serde_json::Value> {
    use serde_json::json;
    let code_types = if cfg!(windows) { json!(["python", "powershell"]) } else { json!(["python", "bash"]) };
    let shell = if cfg!(windows) { "powershell" } else { "bash" };
    vec![
        json!({"type": "function", "function": {
            "name": "code_run",
            "description": format!("代码执行器。优先使用python，仅在必要系统操作时使用 {}。注意：执行的代码必须放在在回复正文中，以 ```python 或 ```{} 代码块的形式。严禁在代码中硬编码大量数据，如有需要应通过文件读取。", shell, shell),
            "parameters": {"type": "object", "properties": {
                "type": {"type": "string", "enum": code_types, "description": "执行环境类型，默认为 python。", "default": "python"},
                "timeout": {"type": "integer", "description": "执行超时时间（秒），默认 60。", "default": 60},
                "cwd": {"type": "string", "description": "工作目录，默认为当前工作目录。"}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "file_read",
            "description": "读取文件内容。建议在修改文件前先读取，以确保获取最新的上下文和行号。支持分页读取或关键字搜索。",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "文件相对或绝对路径。"},
                "start": {"type": "integer", "description": "起始行号（从 1 开始）。", "default": 1},
                "count": {"type": "integer", "description": "读取的行数。", "default": 200},
                "keyword": {"type": "string", "description": "可选搜索关键字。"},
                "show_linenos": {"type": "boolean", "description": "是否显示行号。", "default": true}
            }, "required": ["path"]}
        }}),
        json!({"type": "function", "function": {
            "name": "file_patch",
            "description": "精细化局部文件修改。在文件中寻找唯一的 old_content 块并替换为 new_content。",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "old_content": {"type": "string"},
                "new_content": {"type": "string"}
            }, "required": ["path", "old_content", "new_content"]}
        }}),
        json!({"type": "function", "function": {
            "name": "file_write",
            "description": "用于文件的新建、全量覆盖或追加写入。注意：要写入的内容必须放在回复正文的 <file_content> 标签或代码块中。",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string"},
                "mode": {"type": "string", "enum": ["overwrite", "append", "prepend"], "default": "overwrite"}
            }, "required": ["path"]}
        }}),
        json!({"type": "function", "function": {
            "name": "web_scan",
            "description": "获取当前页面的简化HTML内容和标签页列表。",
            "parameters": {"type": "object", "properties": {
                "tabs_only": {"type": "boolean", "default": false},
                "switch_tab_id": {"type": "string"}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "web_execute_js",
            "description": "万能网页操控工具。通过执行 JavaScript 脚本实现对浏览器的完全控制。",
            "parameters": {"type": "object", "properties": {
                "script": {"type": "string"},
                "save_to_file": {"type": "string", "default": ""},
                "no_monitor": {"type": "boolean", "default": false}
            }, "required": ["script"]}
        }}),
        json!({"type": "function", "function": {
            "name": "update_working_checkpoint",
            "description": "短期工作便签，每轮自动注入上下文，防长任务信息丢失。",
            "parameters": {"type": "object", "properties": {
                "key_info": {"type": "string"},
                "related_sop": {"type": "string"}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "ask_user",
            "description": "当需要用户决策、提供额外信息或遇到无法自动解决的阻碍时，调用此工具中断任务并提问。",
            "parameters": {"type": "object", "properties": {
                "question": {"type": "string"},
                "candidates": {"type": "array", "items": {"type": "string"}}
            }, "required": ["question"]}
        }}),
        json!({"type": "function", "function": {
            "name": "start_long_term_update",
            "description": "准备开始提炼记忆。发现值得长期记忆的信息时调用此工具。超15轮完成的任务必须调用以沉淀经验。",
            "parameters": {"type": "object", "properties": {}}
        }}),
    ]
}

/// High-level async API for embedding the agent in other applications.
/// This is what Android/iOS bindings use.
pub struct AgentSession {
    config: AppConfig,
    work_dir: std::path::PathBuf,
    history: Vec<String>,
    key_info: String,
    related_sop: String,
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
            related_sop: String::new(),
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

        // Restore session state from previous task
        handler.key_info = self.key_info.clone();
        handler.related_sop = self.related_sop.clone();
        handler.history_info = self.history.clone();

        // Build system prompt (matches Python's get_system_prompt())
        let system_prompt = build_system_prompt(&work_dir_str);

        // Full tools schema matching Python original
        let tools = full_tools_schema();

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
        self.related_sop = handler.related_sop.clone();
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
