mod agent_loop;
mod handler;
mod llm;
mod tools;
mod webdriver;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use agent_loop::{agent_runner_loop, AgentResult};
use handler::GenericAgentHandler;
use llm::{AppConfig, ToolClient};

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

/// Default tools schema for the generic agent
fn default_tools_schema() -> Vec<Value> {
    vec![
        json!({
            "name": "code_run",
            "description": "执行Python或Bash代码",
            "parameters": {
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python", "bash"],
                        "description": "编程语言"
                    },
                    "code": {
                        "type": "string",
                        "description": "要执行的代码"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "超时秒数（默认30）"
                    }
                },
                "required": ["language", "code"]
            }
        }),
        json!({
            "name": "file_read",
            "description": "读取文件内容，支持行号、关键词搜索和分页",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "文件路径"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "起始行（1-based）"
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "结束行（1-based）"
                    },
                    "keyword": {
                        "type": "string",
                        "description": "搜索关键词"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "file_patch",
            "description": "在文件中找到唯一的旧内容并替换为新内容",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "文件路径"
                    },
                    "old_content": {
                        "type": "string",
                        "description": "要替换的原文本（必须在文件中唯一）"
                    },
                    "new_content": {
                        "type": "string",
                        "description": "替换后的新文本"
                    }
                },
                "required": ["path", "old_content", "new_content"]
            }
        }),
        json!({
            "name": "file_write",
            "description": "写入、追加或前置文件内容",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "文件路径"
                    },
                    "content": {
                        "type": "string",
                        "description": "文件内容（支持<file_content>标签或代码块）"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append", "prepend"],
                        "description": "写入模式（默认：overwrite）"
                    }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "web_scan",
            "description": "获取网页的简化HTML内容",
            "parameters": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "要访问的URL"
                    }
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "web_execute_js",
            "description": "在浏览器中执行JavaScript",
            "parameters": {
                "type": "object",
                "properties": {
                    "js": {
                        "type": "string",
                        "description": "要执行的JavaScript代码"
                    }
                },
                "required": ["js"]
            }
        }),
        json!({
            "name": "ask_user",
            "description": "中断任务并向用户提问",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "要问用户的问题"
                    }
                },
                "required": ["question"]
            }
        }),
        json!({
            "name": "update_working_checkpoint",
            "description": "更新工作记忆检查点",
            "parameters": {
                "type": "object",
                "properties": {
                    "key_info": {
                        "type": "string",
                        "description": "要保存的关键信息"
                    }
                },
                "required": ["key_info"]
            }
        }),
        json!({
            "name": "start_long_term_update",
            "description": "触发长期记忆整合",
            "parameters": {
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "要整合的内容摘要"
                    }
                },
                "required": ["content"]
            }
        }),
    ]
}

/// System prompt for the generic agent
fn default_system_prompt(task: &str, work_dir: &str) -> String {
    format!(
        r#"你是一个强大的AI代理，能够通过工具完成各种计算机任务。

## 当前任务
{task}

## 工作目录
{work_dir}

## 行为准则
1. 首先分析任务，制定清晰的执行计划
2. 逐步执行，每步都要验证结果
3. 遇到错误时，分析原因并修正
4. 保持代码整洁、有注释
5. 完成任务后，给出简洁的总结报告

## 注意事项
- 不要对用户撒谎
- 不要执行危险操作（rm -rf /等）
- 如有不确定的地方，使用ask_user工具询问用户
"#,
        task = task,
        work_dir = work_dir
    )
}

/// Run the agent loop, printing chunks to stdout
async fn run_agent(
    config: AppConfig,
    task: &str,
    work_dir: &str,
    max_turns: usize,
    verbose: bool,
) -> Result<AgentResult> {
    let mut client = ToolClient::new(config)?;
    let mut handler = GenericAgentHandler::new(task, work_dir);
    let tools = default_tools_schema();
    let system_prompt = default_system_prompt(task, work_dir);

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
        let driver = Arc::new(webdriver::TMWebDriver::new(*port));
        driver.start().await?;
        return Ok(());
    }

    // Load config
    let config = load_config(cli.config.as_deref())?;

    // Start webdriver server if requested
    if cli.webdriver {
        let driver = Arc::new(webdriver::TMWebDriver::new(cli.webdriver_port));
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
