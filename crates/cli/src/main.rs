use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use pc_agent_loop_core::{agent_runner_loop, AgentResult, AppConfig, GenericAgentHandler, ToolClient};
use pc_agent_loop_core::webdriver::TMWebDriver;

/// CLI arguments
#[derive(Parser, Debug)]
#[command(name = "pc-agent-loop", about = "AI Agent Loop (Rust port of pc-agent-loop-py)")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Task description (interactive mode if not provided)
    #[arg(short, long)]
    task: Option<String>,

    /// Working directory
    #[arg(short, long, default_value = ".")]
    work_dir: String,

    /// Config file path (default: ./mykey.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Maximum turns
    #[arg(short, long, default_value = "15")]
    max_turns: usize,

    /// Verbose mode (stream LLM output in real time)
    #[arg(short, long)]
    verbose: bool,

    /// Start WebDriver server
    #[arg(long)]
    webdriver: bool,

    /// WebDriver port
    #[arg(long, default_value = "9999")]
    webdriver_port: u16,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run agent with a task from an IO directory
    Task {
        /// IO directory containing task.txt
        dir: PathBuf,
    },
    /// Run agent in scheduled mode
    Scheduled {
        /// Tasks file
        #[arg(short, long)]
        tasks_file: Option<PathBuf>,
    },
    /// Start the WebDriver server only
    Webdriver {
        #[arg(short, long, default_value = "9999")]
        port: u16,
    },
}

/// Load config from mykey.json
fn load_config(config_path: Option<&Path>) -> Result<AppConfig> {
    let paths_to_try = if let Some(p) = config_path {
        vec![p.to_path_buf()]
    } else {
        vec![
            PathBuf::from("mykey.json"),
            PathBuf::from("./mykey.json"),
            dirs_home().map(|h| h.join(".config/pc-agent/mykey.json")).unwrap_or_default(),
        ]
    };

    for path in &paths_to_try {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let config: AppConfig = serde_json::from_str(&content)
                .map_err(|e| anyhow!("Failed to parse config {}: {}", path.display(), e))?;
            info!("Loaded config from {}", path.display());
            return Ok(config);
        }
    }

    Err(anyhow!(
        "No config file found. Create mykey.json with oai_config or claude_config.\n\
         Example:\n{}\n\nTried paths: {:?}",
        serde_json::to_string_pretty(&json!({
            "oai_config": {
                "apikey": "sk-...",
                "apibase": "https://api.openai.com",
                "model": "gpt-4o"
            },
            "proxy": "http://127.0.0.1:7890"
        })).unwrap(),
        paths_to_try
    ))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Tools schema matching the Python original (assets/tools_schema.json).
/// On non-Windows, "powershell" is replaced with "bash".
pub fn default_tools_schema() -> Vec<Value> {
    let code_types = if cfg!(windows) {
        json!(["python", "powershell", "lua", "javascript"])
    } else {
        json!(["python", "bash", "lua", "javascript"])
    };
    let code_type_desc = if cfg!(windows) { "powershell" } else { "bash" };

    vec![
        json!({"type": "function", "function": {
            "name": "code_run",
            "description": format!("代码执行器。优先使用python，仅在必要系统操作时使用 {}。支持 lua/javascript(node)。注意：执行的代码必须放在在回复正文中，以 ```python 或 ```{} 代码块的形式。严禁在代码中硬编码大量数据，如有需要应通过文件读取。", code_type_desc, code_type_desc),
            "parameters": {"type": "object", "properties": {
                "type": {"type": "string", "enum": code_types, "description": "执行环境类型，默认为 python。支持 python/bash/lua/javascript。", "default": "python"},
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
                "keyword": {"type": "string", "description": "可选搜索关键字。如果提供，将返回第一个匹配项（忽略大小写）及其周边的内容。"},
                "show_linenos": {"type": "boolean", "description": "是否显示行号，建议开启以辅助 file_patch 定位。", "default": true}
            }, "required": ["path"]}
        }}),
        json!({"type": "function", "function": {
            "name": "file_patch",
            "description": "精细化局部文件修改。在文件中寻找唯一的 old_content 块并替换为 new_content。要求 old_content 必须在文件中唯一存在，且空格、缩进、换行必须与原文件完全一致。如果匹配失败，请使用 file_read 重新确认文件内容。",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "文件路径。"},
                "old_content": {"type": "string", "description": "文件中需要被替换的原始文本块（需确保唯一性）。"},
                "new_content": {"type": "string", "description": "替换后的新文本内容。"}
            }, "required": ["path", "old_content", "new_content"]}
        }}),
        json!({"type": "function", "function": {
            "name": "file_write",
            "description": "用于文件的新建、全量覆盖或追加写入。对于精细的代码修改，应优先使用 file_patch。注意：要写入的内容必须放在回复正文的 <file_content> 标签或代码块中。",
            "parameters": {"type": "object", "properties": {
                "path": {"type": "string", "description": "文件路径。"},
                "mode": {"type": "string", "enum": ["overwrite", "append", "prepend"], "description": "写入模式覆盖、追加或在开头追加。", "default": "overwrite"}
            }, "required": ["path"]}
        }}),
        json!({"type": "function", "function": {
            "name": "web_scan",
            "description": "获取当前页面的简化HTML内容和标签页列表。注意：简化会过滤边栏、浮动元素等非主体内容，如需查看被过滤内容请用execute_js。切换页面后一般应先调用查看。",
            "parameters": {"type": "object", "properties": {
                "tabs_only": {"type": "boolean", "description": "仅返回标签页列表和当前标签信息，不获取HTML内容。", "default": false},
                "switch_tab_id": {"type": "string", "description": "可选的标签页 ID。如果提供，系统将在扫描前切换到该标签页。"}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "web_execute_js",
            "description": "万能网页操控工具。通过执行 JavaScript 脚本实现对浏览器的完全控制（如点击、滚动、提取特定数据）。鼓励在有把握情况下（记忆中有selector/做法等）精准使用以减少web_scan调用。执行结果可选择保存到本地文件进行后续分析。",
            "parameters": {"type": "object", "properties": {
                "script": {"type": "string", "description": "要执行的 JavaScript 代码或JS文件路径。"},
                "save_to_file": {"type": "string", "description": "结果存文件，适合返回值较长时。不支持await。", "default": ""},
                "no_monitor": {"type": "boolean", "description": "跳过页面变更监控，省2-3秒。仅在纯读取信息时设置，页面操作时不要设置。", "default": false}
            }, "required": ["script"]}
        }}),
        json!({"type": "function", "function": {
            "name": "update_working_checkpoint",
            "description": "短期工作便签，每轮自动注入上下文，防长任务信息丢失。前中期调用，非结束时。何时调用：(1)任务开始读SOP后，存用户需求和关键约束/参数（简单1-2步任务除外）；(2)子任务切换或上下文即将被冲刷前；(3)多次重试失败后，重读SOP并必须调用存储新发现；(4)切换新任务时更新内容，清旧进度但保留仍有效的约束。\n\n何时不调用：简单任务（1-2步且无严重约束）、任务已完成时（应当用长期结算工具）。",
            "parameters": {"type": "object", "properties": {
                "key_info": {"type": "string", "description": "替换当前便签（<200 tokens）。增量更新：先回顾现有内容，保留仍有效的，再增删改。存：要避的坑、用户原始需求、关键参数/发现、文件路径、当前进度、下一步计划。不存：马上要用用完即丢的、上下文中显而易见的、用户已换全新任务时的旧任务信息。宁多更新不丢关键。"},
                "related_sop": {"type": "string", "description": "相关sop名称，可以多个，必要时需要再读"}
            }}
        }}),
        json!({"type": "function", "function": {
            "name": "ask_user",
            "description": "当需要用户决策、提供额外信息或遇到无法自动解决的阻碍时，调用此工具中断任务并提问。",
            "parameters": {"type": "object", "properties": {
                "question": {"type": "string", "description": "向用户提出的明确问题。"},
                "candidates": {"type": "array", "items": {"type": "string"}, "description": "提供给用户的可选快捷选项列表。"}
            }, "required": ["question"]}
        }}),
        json!({"type": "function", "function": {
            "name": "start_long_term_update",
            "description": "准备开始提炼记忆。发现值得长期记忆的信息（环境事实/用户偏好/避坑经验）时调用此工具。已记忆更新或在自主流程内时无需调用。超15轮完成的任务必须调用以沉淀经验。",
            "parameters": {"type": "object", "properties": {}}
        }}),
    ]
}

/// Build the system prompt, matching Python's `get_system_prompt()`.
/// Loads from assets/sys_prompt.txt if present, otherwise uses the built-in prompt.
fn default_system_prompt(work_dir: &str) -> String {
    use std::path::Path;

    // Try to load from assets/sys_prompt.txt (matches Python)
    let assets_prompt = Path::new(work_dir).join("assets").join("sys_prompt.txt");
    let base_prompt = if assets_prompt.exists() {
        std::fs::read_to_string(&assets_prompt).unwrap_or_else(|_| builtin_sys_prompt())
    } else {
        builtin_sys_prompt()
    };

    let today = chrono::Local::now().format("%Y-%m-%d %a").to_string();
    let global_memory = load_global_memory(work_dir);

    format!("{}\nToday: {}\n{}", base_prompt, today, global_memory)
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

fn load_global_memory(work_dir: &str) -> String {
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

/// Ensure required work_dir subdirectories exist (matches Python's agentmain.py init).
fn init_work_dir(work_dir: &str) {
    use std::path::Path;
    let base = Path::new(work_dir);
    let _ = std::fs::create_dir_all(base.join("memory"));
    let _ = std::fs::create_dir_all(base.join("temp"));

    // Initialize global_mem_insight.txt from template if missing
    let insight = base.join("memory").join("global_mem_insight.txt");
    if !insight.exists() {
        let template = base.join("assets").join("global_mem_insight_template.txt");
        let content = std::fs::read_to_string(&template).unwrap_or_default();
        let _ = std::fs::write(&insight, content);
    }
    // Initialize global_mem.txt if missing
    let mem_txt = base.join("memory").join("global_mem.txt");
    if !mem_txt.exists() {
        let _ = std::fs::write(&mem_txt, "");
    }
}

/// Run the agent loop, printing chunks to stdout
async fn run_agent(
    config: AppConfig,
    task: &str,
    work_dir: &str,
    max_turns: usize,
    verbose: bool,
) -> Result<AgentResult> {
    init_work_dir(work_dir);
    let mut client = ToolClient::new(config)?;
    let mut handler = GenericAgentHandler::new(task, work_dir);
    let tools = default_tools_schema();
    let system_prompt = default_system_prompt(work_dir);

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Spawn a task to print output chunks
    let print_task = tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            print!("{}", chunk);
        }
    });

    let result = agent_runner_loop(
        &mut client,
        &system_prompt,
        task,
        &mut handler,
        &tools,
        max_turns,
        verbose,
        &tx,
    )
    .await;

    // Signal end of output
    drop(tx);
    let _ = print_task.await;

    Ok(result)
}

/// GeneraticAgent: multi-backend agent with task queue.
/// Mirrors Python's GeneraticAgent class.
pub struct GeneraticAgent {
    pub config: AppConfig,
    pub work_dir: String,
    pub max_turns: usize,
    pub verbose: bool,
}

impl GeneraticAgent {
    pub fn new(config: AppConfig, work_dir: &str, max_turns: usize, verbose: bool) -> Self {
        GeneraticAgent {
            config,
            work_dir: work_dir.to_string(),
            max_turns,
            verbose,
        }
    }

    /// Run a single task
    pub async fn run_task(&self, task: &str) -> Result<AgentResult> {
        run_agent(
            self.config.clone(),
            task,
            &self.work_dir,
            self.max_turns,
            self.verbose,
        )
        .await
    }

    /// Run tasks from an IO directory (reads task.txt, writes result.txt)
    pub async fn run_task_from_dir(&self, dir: &Path) -> Result<()> {
        let task_file = dir.join("task.txt");
        let result_file = dir.join("result.txt");

        if !task_file.exists() {
            return Err(anyhow!("task.txt not found in {}", dir.display()));
        }

        let task = tokio::fs::read_to_string(&task_file).await?;
        let task = task.trim();

        info!("Running task from {}: {:.80}", dir.display(), task);

        let result = self.run_task(task).await?;

        let result_str = match &result {
            AgentResult::CurrentTaskDone(Some(data)) => {
                format!("DONE\n{}", serde_json::to_string_pretty(data).unwrap_or_default())
            }
            AgentResult::CurrentTaskDone(None) => "DONE".to_string(),
            AgentResult::Exited(Some(data)) => {
                format!("EXITED\n{}", serde_json::to_string_pretty(data).unwrap_or_default())
            }
            AgentResult::Exited(None) => "EXITED".to_string(),
            AgentResult::MaxTurnsExceeded => "MAX_TURNS_EXCEEDED".to_string(),
            AgentResult::Error(e) => format!("ERROR\n{}", e),
        };

        tokio::fs::write(&result_file, result_str).await?;
        info!("Result written to {}", result_file.display());

        Ok(())
    }

    /// Interactive CLI mode
    pub async fn run_interactive(&self) -> Result<()> {
        use std::io::{self, BufRead, Write};

        println!("PC Agent Loop (Rust) - Interactive Mode");
        println!("Type your task and press Enter. Type 'quit' to exit.");
        println!("Working directory: {}", self.work_dir);
        println!();

        let stdin = io::stdin();
        loop {
            print!("Task> ");
            io::stdout().flush()?;

            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break; // EOF
            }

            let task = line.trim();
            if task.is_empty() {
                continue;
            }
            if task == "quit" || task == "exit" {
                break;
            }

            println!("\n--- Running agent ---");
            match self.run_task(task).await {
                Ok(result) => {
                    println!("\n--- Result: {:?} ---\n", result);
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                }
            }
        }

        println!("Goodbye!");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // Handle webdriver-only subcommand first
    if let Some(Commands::Webdriver { port }) = &cli.command {
        info!("Starting WebDriver server on port {}", port);
        let driver = Arc::new(TMWebDriver::new(*port));
        driver.start().await?;
        return Ok(());
    }

    // Load config
    let config = load_config(cli.config.as_deref())?;

    // Start webdriver server if requested
    if cli.webdriver {
        let driver = Arc::new(TMWebDriver::new(cli.webdriver_port));
        let driver_clone = driver.clone();
        tokio::spawn(async move {
            if let Err(e) = driver_clone.start().await {
                warn!("WebDriver server error: {}", e);
            }
        });
        info!("WebDriver server started on port {}", cli.webdriver_port);
    }

    let agent = GeneraticAgent::new(config, &cli.work_dir, cli.max_turns, cli.verbose);

    match &cli.command {
        Some(Commands::Task { dir }) => {
            agent.run_task_from_dir(dir).await?;
        }
        Some(Commands::Scheduled { tasks_file }) => {
            // Scheduled mode: watch for task files
            let tasks_file = tasks_file.clone().unwrap_or_else(|| PathBuf::from("tasks.json"));
            run_scheduled_mode(&agent, &tasks_file).await?;
        }
        Some(Commands::Webdriver { .. }) => {
            // Already handled above
        }
        None => {
            // Interactive or --task mode
            if let Some(task) = &cli.task {
                println!("Running task: {}", task);
                let result = agent.run_task(task).await?;
                println!("\nResult: {:?}", result);
            } else {
                agent.run_interactive().await?;
            }
        }
    }

    Ok(())
}

/// Scheduled mode: poll a tasks file for new tasks
async fn run_scheduled_mode(agent: &GeneraticAgent, tasks_file: &Path) -> Result<()> {
    use tokio::time::{interval, Duration};

    info!("Scheduled mode: watching {}", tasks_file.display());
    let mut ticker = interval(Duration::from_secs(5));

    loop {
        ticker.tick().await;

        if !tasks_file.exists() {
            continue;
        }

        let content = match tokio::fs::read_to_string(tasks_file).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read tasks file: {}", e);
                continue;
            }
        };

        let tasks: Vec<Value> = match serde_json::from_str(&content) {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to parse tasks file: {}", e);
                continue;
            }
        };

        for task in tasks {
            let task_str = match task.get("task")
                .or_else(|| task.get("description"))
                .and_then(|t| t.as_str())
            {
                Some(s) => s.to_string(),
                None => continue,
            };

            let status = task.get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("pending");

            if status != "pending" {
                continue;
            }

            info!("Running scheduled task: {:.80}", task_str);
            match agent.run_task(&task_str).await {
                Ok(result) => info!("Task result: {:?}", result),
                Err(e) => warn!("Task error: {}", e),
            }
        }
    }
}
