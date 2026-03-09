use axum::{
    extract::{Path as AxumPath, State},
    response::{
        sse::{Event, KeepAlive},
        Html, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{info, warn};
use uuid::Uuid;

use pc_agent_loop_core::llm::types::AppConfig;

// ---------------------------------------------------------------------------
// Embedded HTML interface
// ---------------------------------------------------------------------------

const HTML_UI: &str = r#"<!DOCTYPE html>
<html>
<head>
  <title>Cowork Agent</title>
  <meta charset="utf-8">
  <style>
    * { box-sizing: border-box; }
    body { background: #1e1e2e; color: #cdd6f4; font-family: monospace; margin: 0; display: flex; height: 100vh; overflow: hidden; }
    #sidebar { width: 220px; background: #181825; padding: 16px; border-right: 1px solid #313244; display: flex; flex-direction: column; gap: 12px; flex-shrink: 0; }
    #main { flex: 1; display: flex; flex-direction: column; overflow: hidden; }
    #messages { flex: 1; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 12px; }
    .msg { border-radius: 8px; padding: 12px; max-width: 90%; white-space: pre-wrap; line-height: 1.5; word-break: break-word; }
    .user { background: #313244; align-self: flex-end; color: #cdd6f4; }
    .agent { background: #1e1e2e; border: 1px solid #313244; align-self: flex-start; color: #cdd6f4; }
    .agent pre { background: #11111b; padding: 8px; border-radius: 4px; overflow-x: auto; margin: 4px 0; }
    .agent code.inline { background: #313244; padding: 2px 4px; border-radius: 3px; font-size: 13px; }
    #input-area { padding: 16px; border-top: 1px solid #313244; display: flex; gap: 8px; flex-shrink: 0; }
    #input { flex: 1; background: #313244; border: 1px solid #45475a; color: #cdd6f4; padding: 10px; border-radius: 6px; font-size: 14px; font-family: monospace; resize: none; height: 60px; outline: none; }
    #input:focus { border-color: #89b4fa; }
    button { background: #89b4fa; color: #1e1e2e; border: none; padding: 8px 16px; border-radius: 6px; cursor: pointer; font-weight: bold; font-family: monospace; font-size: 13px; }
    button:hover:not(:disabled) { background: #74c7ec; }
    button.danger { background: #f38ba8; }
    button.danger:hover:not(:disabled) { background: #eba0ac; }
    button:disabled { opacity: 0.5; cursor: not-allowed; }
    .status { font-size: 12px; color: #6c7086; }
    .status.running { color: #a6e3a1; }
    h3 { color: #cba6f7; margin: 0; font-size: 14px; }
    hr { border: none; border-top: 1px solid #313244; margin: 4px 0; }
    .spinner { display: inline-block; animation: spin 1s linear infinite; }
    @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
    #turn-info { font-size: 12px; color: #6c7086; }
  </style>
</head>
<body>
  <div id="sidebar">
    <h3>&#x1F5A5; Cowork Agent</h3>
    <div class="status" id="llm-name">LLM: ...</div>
    <div class="status" id="status-text">&#x25CF; 空闲</div>
    <button onclick="switchLlm()">切换 LLM</button>
    <button class="danger" onclick="abortTask()" id="abort-btn" disabled>&#x23F9; 中止任务</button>
    <hr>
    <div id="turn-info">Turn: 0</div>
  </div>
  <div id="main">
    <div id="messages"></div>
    <div id="input-area">
      <textarea id="input" placeholder="输入任务... (Ctrl+Enter 发送)"></textarea>
      <button onclick="sendTask()" id="send-btn">发送</button>
    </div>
  </div>
  <script>
    let currentEventSource = null;
    let isRunning = false;

    // Poll status periodically
    function pollStatus() {
      fetch('/api/status')
        .then(r => r.json())
        .then(data => {
          document.getElementById('llm-name').textContent = 'LLM: ' + (data.llm_name || '?');
          document.getElementById('turn-info').textContent = 'Turn: ' + (data.current_turn || 0);
          const statusEl = document.getElementById('status-text');
          if (data.is_running) {
            statusEl.textContent = '\u23F3 运行中...';
            statusEl.className = 'status running';
          } else {
            statusEl.textContent = '\u25CF 空闲';
            statusEl.className = 'status';
          }
        })
        .catch(() => {});
    }
    setInterval(pollStatus, 2000);
    pollStatus();

    function renderMarkdown(text) {
      // Escape HTML first
      let html = text
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');

      // Code blocks (```...```)
      html = html.replace(/```(\w*)\n?([\s\S]*?)```/g, function(_, lang, code) {
        return '<pre><code>' + code + '</code></pre>';
      });

      // Inline code (`...`)
      html = html.replace(/`([^`\n]+)`/g, '<code class="inline">$1</code>');

      // Bold (**text**)
      html = html.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');

      // Italic (*text*)
      html = html.replace(/\*([^*\n]+)\*/g, '<em>$1</em>');

      // Newlines to <br> (but not inside pre blocks - those are already formatted)
      // We handle this by splitting on pre blocks
      const parts = html.split(/(<pre>[\s\S]*?<\/pre>)/);
      html = parts.map((part, i) => {
        if (i % 2 === 0) {
          return part.replace(/\n/g, '<br>');
        }
        return part;
      }).join('');

      return html;
    }

    function appendMessage(role, text) {
      const msgs = document.getElementById('messages');
      const div = document.createElement('div');
      div.className = 'msg ' + role;
      if (role === 'agent') {
        div.innerHTML = renderMarkdown(text);
      } else {
        div.textContent = text;
      }
      msgs.appendChild(div);
      msgs.scrollTop = msgs.scrollHeight;
      return div;
    }

    function updateAgentMessage(div, text) {
      div.innerHTML = renderMarkdown(text);
      const msgs = document.getElementById('messages');
      msgs.scrollTop = msgs.scrollHeight;
    }

    function setRunning(running) {
      isRunning = running;
      document.getElementById('send-btn').disabled = running;
      document.getElementById('abort-btn').disabled = !running;
      const statusEl = document.getElementById('status-text');
      if (running) {
        statusEl.textContent = '\u23F3 运行中...';
        statusEl.className = 'status running';
      } else {
        statusEl.textContent = '\u25CF 空闲';
        statusEl.className = 'status';
      }
    }

    async function sendTask() {
      const input = document.getElementById('input');
      const query = input.value.trim();
      if (!query || isRunning) return;

      input.value = '';
      appendMessage('user', query);
      setRunning(true);

      let agentDiv = null;
      let agentText = '';

      try {
        const resp = await fetch('/api/task', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query })
        });
        const data = await resp.json();
        const taskId = data.task_id;

        agentDiv = appendMessage('agent', '\u23F3 启动中...');
        agentText = '';

        if (currentEventSource) {
          currentEventSource.close();
        }

        const es = new EventSource('/api/stream/' + taskId);
        currentEventSource = es;

        es.onmessage = function(e) {
          const chunk = e.data;
          if (chunk === '__DONE__') {
            es.close();
            currentEventSource = null;
            setRunning(false);
            return;
          }
          agentText += chunk;
          updateAgentMessage(agentDiv, agentText);
        };

        es.onerror = function() {
          es.close();
          currentEventSource = null;
          setRunning(false);
        };

      } catch (err) {
        if (agentDiv) {
          updateAgentMessage(agentDiv, 'Error: ' + err.message);
        }
        setRunning(false);
      }
    }

    async function abortTask() {
      try {
        await fetch('/api/abort', { method: 'POST' });
      } catch (e) {}
      if (currentEventSource) {
        currentEventSource.close();
        currentEventSource = null;
      }
      setRunning(false);
    }

    async function switchLlm() {
      try {
        const resp = await fetch('/api/llm/next', { method: 'POST' });
        const data = await resp.json();
        document.getElementById('llm-name').textContent = 'LLM: ' + (data.llm_name || '?');
      } catch (e) {}
    }

    // Ctrl+Enter to submit
    document.getElementById('input').addEventListener('keydown', function(e) {
      if (e.ctrlKey && e.key === 'Enter') {
        sendTask();
      }
    });
  </script>
</body>
</html>"#;

// ---------------------------------------------------------------------------
// State management
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<AgentState>>,
}

struct AgentState {
    config: AppConfig,
    work_dir: String,
    is_running: bool,
    current_turn: usize,
    llm_name: String,
    abort_tx: Option<oneshot::Sender<()>>,
    /// Per-task output chunks; key = task_id
    task_outputs: HashMap<String, Vec<String>>,
    /// Tasks that are finished
    task_done: HashSet<String>,
}

impl AgentState {
    fn new(config: AppConfig, work_dir: String) -> Self {
        let llm_name = Self::detect_llm_name(&config);
        AgentState {
            config,
            work_dir,
            is_running: false,
            current_turn: 0,
            llm_name,
            abort_tx: None,
            task_outputs: HashMap::new(),
            task_done: HashSet::new(),
        }
    }

    fn detect_llm_name(config: &AppConfig) -> String {
        if let Some(claude) = &config.claude_config {
            return format!("Claude/{}", claude.model);
        }
        if let Some(oai) = &config.oai_config {
            return format!("OAI/{}", oai.model);
        }
        "Unknown".to_string()
    }
}

// ---------------------------------------------------------------------------
// API request/response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TaskRequest {
    query: String,
}

#[derive(Serialize)]
struct TaskResponse {
    task_id: String,
}

#[derive(Serialize)]
struct StatusResponse {
    is_running: bool,
    llm_name: String,
    current_turn: usize,
}

#[derive(Serialize)]
struct LlmResponse {
    llm_name: String,
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

async fn get_index() -> Html<&'static str> {
    Html(HTML_UI)
}

async fn post_task(
    State(state): State<AppState>,
    Json(req): Json<TaskRequest>,
) -> Json<TaskResponse> {
    let task_id = Uuid::new_v4().to_string();
    let query = req.query.clone();

    {
        let mut s = state.inner.lock().await;
        s.task_outputs.insert(task_id.clone(), Vec::new());
        s.task_done.remove(&task_id);
        s.is_running = true;
        s.current_turn = 0;
    }

    let state_clone = state.clone();
    let task_id_clone = task_id.clone();

    tokio::spawn(async move {
        run_agent_task(state_clone, task_id_clone, query).await;
    });

    Json(TaskResponse { task_id })
}

async fn run_agent_task(state: AppState, task_id: String, query: String) {
    use pc_agent_loop_core::{
        agent_loop::agent_runner_loop,
        handler::GenericAgentHandler,
        llm::ToolClient,
        build_system_prompt,
        full_tools_schema,
    };

    let (config, work_dir) = {
        let s = state.inner.lock().await;
        (s.config.clone(), s.work_dir.clone())
    };

    // Create an mpsc channel for streaming chunks
    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<String>();

    // Create abort channel
    let (abort_tx, abort_rx) = oneshot::channel::<()>();
    {
        let mut s = state.inner.lock().await;
        s.abort_tx = Some(abort_tx);
    }

    // Initialize work dir structure (memory/, temp/, etc.)
    {
        let base = std::path::Path::new(&work_dir);
        let _ = std::fs::create_dir_all(base.join("memory"));
        let _ = std::fs::create_dir_all(base.join("temp"));
        let insight = base.join("memory").join("global_mem_insight.txt");
        if !insight.exists() { let _ = std::fs::write(&insight, ""); }
    }

    // Spawn the actual agent loop in a task
    let chunk_tx_clone = chunk_tx.clone();
    let work_dir_clone = work_dir.clone();
    let agent_handle = tokio::spawn(async move {
        let mut client = match ToolClient::new(config) {
            Ok(c) => c,
            Err(e) => {
                let _ = chunk_tx_clone.send(format!("**Error creating LLM client: {}**\n", e));
                return;
            }
        };

        let mut handler = GenericAgentHandler::new(&query, &work_dir_clone);

        let system_prompt_str = build_system_prompt(&work_dir_clone);
        let tools_schema = full_tools_schema();

        let _result = agent_runner_loop(
            &mut client,
            &system_prompt_str,
            &query,
            &mut handler,
            &tools_schema,
            50,
            false,
            &chunk_tx_clone,
        ).await;

        let _ = chunk_tx_clone.send("__DONE__".to_string());
    });

    // Forward chunks from the agent to the task output store, respecting abort
    tokio::select! {
        _ = async {
            while let Some(chunk) = chunk_rx.recv().await {
                let is_done = chunk == "__DONE__";
                {
                    let mut s = state.inner.lock().await;
                    s.task_outputs.entry(task_id.clone()).or_default().push(chunk.clone());
                    if is_done {
                        s.task_done.insert(task_id.clone());
                        s.is_running = false;
                        s.abort_tx = None;
                    }
                }
                if is_done { break; }
            }
        } => {}
        _ = abort_rx => {
            agent_handle.abort();
            let mut s = state.inner.lock().await;
            s.task_outputs.entry(task_id.clone()).or_default().push("\n**[任务已中止]**\n".to_string());
            s.task_outputs.entry(task_id.clone()).or_default().push("__DONE__".to_string());
            s.task_done.insert(task_id.clone());
            s.is_running = false;
            s.abort_tx = None;
        }
    }
}

async fn get_stream(
    State(state): State<AppState>,
    AxumPath(task_id): AxumPath<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut sent_index = 0usize;

        loop {
            let (chunks_snapshot, is_done) = {
                let s = state.inner.lock().await;
                let chunks = s.task_outputs.get(&task_id).cloned().unwrap_or_default();
                let done = s.task_done.contains(&task_id);
                (chunks, done)
            };

            // Send any new chunks since last time
            while sent_index < chunks_snapshot.len() {
                let chunk = &chunks_snapshot[sent_index];
                yield Ok(Event::default().data(chunk.as_str()));
                sent_index += 1;
            }

            if is_done && sent_index >= chunks_snapshot.len() {
                break;
            }

            // Small sleep to avoid busy-looping
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn post_abort(State(state): State<AppState>) -> Json<Value> {
    let mut s = state.inner.lock().await;
    if let Some(tx) = s.abort_tx.take() {
        let _ = tx.send(());
        info!("Abort signal sent");
    }
    Json(serde_json::json!({"ok": true}))
}

async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    let s = state.inner.lock().await;
    Json(StatusResponse {
        is_running: s.is_running,
        llm_name: s.llm_name.clone(),
        current_turn: s.current_turn,
    })
}

async fn post_llm_next(State(state): State<AppState>) -> Json<LlmResponse> {
    let mut s = state.inner.lock().await;
    // Cycle between Claude and OAI if both are configured
    let has_claude = s.config.claude_config.is_some();
    let has_oai = s.config.oai_config.is_some();

    if has_claude && has_oai {
        // Toggle by swapping active backend reference in llm_name
        if s.llm_name.starts_with("Claude") {
            if let Some(oai) = &s.config.oai_config {
                s.llm_name = format!("OAI/{}", oai.model);
            }
        } else {
            if let Some(claude) = &s.config.claude_config {
                s.llm_name = format!("Claude/{}", claude.model);
            }
        }
    } else {
        warn!("Only one LLM backend configured, cannot switch");
    }

    Json(LlmResponse { llm_name: s.llm_name.clone() })
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(name = "pc-agent-loop-gui", about = "Web chat interface for pc-agent-loop")]
struct Cli {
    /// Port to listen on
    #[arg(short, long, default_value = "7891")]
    port: u16,

    /// Path to config file (mykey.json)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Working directory for the agent
    #[arg(short, long, default_value = ".")]
    work_dir: String,

    /// Open browser automatically on startup
    #[arg(long)]
    open: bool,
}

fn load_config(path: Option<&PathBuf>) -> anyhow::Result<AppConfig> {
    #[allow(unused_imports)]
    use pc_agent_loop_core::llm::types::{ClaudeConfig, OaiConfig};

    let config_path = if let Some(p) = path {
        p.clone()
    } else {
        // Try default locations
        let candidates = [
            PathBuf::from("mykey.json"),
            PathBuf::from("config.json"),
            dirs_or_home().join("mykey.json"),
        ];
        candidates
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("mykey.json"))
    };

    if config_path.exists() {
        let text = std::fs::read_to_string(&config_path)?;
        let config: AppConfig = serde_json::from_str(&text)?;
        Ok(config)
    } else {
        // Return a default config with no backends; the server will still start
        // but LLM calls will fail with a clear error message
        warn!(
            "Config file {:?} not found. Starting with no LLM backend.",
            config_path
        );
        Ok(AppConfig {
            oai_config: None,
            claude_config: None,
            proxy: None,
        })
    }
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use clap::Parser;
    use tower_http::cors::{Any, CorsLayer};

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = load_config(cli.config.as_ref())?;
    let work_dir = cli.work_dir.clone();

    let state = AppState {
        inner: Arc::new(Mutex::new(AgentState::new(config, work_dir))),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(get_index))
        .route("/api/task", post(post_task))
        .route("/api/stream/:task_id", get(get_stream))
        .route("/api/abort", post(post_abort))
        .route("/api/status", get(get_status))
        .route("/api/llm/next", post(post_llm_next))
        .layer(cors)
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], cli.port));
    let url = format!("http://localhost:{}", cli.port);

    info!("Starting pc-agent-loop-gui at {}", url);
    println!("Cowork Agent GUI running at: {}", url);

    if cli.open {
        let url_clone = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Err(e) = open_browser(&url_clone) {
                warn!("Failed to open browser: {}", e);
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(url).spawn()?;
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(url).spawn()?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn()?;
    Ok(())
}
