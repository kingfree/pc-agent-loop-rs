use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{Mutex, oneshot};
use serde::Serialize;

use pc_agent_loop_core::llm::types::AppConfig;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct Inner {
    config: AppConfig,
    work_dir: String,
    is_running: bool,
    current_turn: usize,
    llm_names: Vec<String>,
    llm_index: usize,
    abort_tx: Option<oneshot::Sender<()>>,
}

type Shared = Arc<Mutex<Inner>>;

struct AppState(Shared);

// ---------------------------------------------------------------------------
// Serializable types returned to the frontend
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusPayload {
    is_running: bool,
    llm_name: String,
    current_turn: usize,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn run_task(
    query: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let shared = state.0.clone();

    {
        let mut s = shared.lock().await;
        if s.is_running {
            return Err("已有任务在运行".into());
        }
        s.is_running = true;
        s.current_turn = 0;
    }

    let (abort_tx, abort_rx) = oneshot::channel::<()>();
    shared.lock().await.abort_tx = Some(abort_tx);

    let shared2 = shared.clone();
    let app2 = app.clone();

    tauri::async_runtime::spawn(async move {
        let (config, work_dir) = {
            let s = shared2.lock().await;
            (s.config.clone(), s.work_dir.clone())
        };

        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let chunk_tx2 = chunk_tx.clone();
        let work_dir2 = work_dir.clone();

        let agent_handle = tauri::async_runtime::spawn(async move {
            use pc_agent_loop_core::{
                agent_runner_loop, build_system_prompt, full_tools_schema,
                handler::GenericAgentHandler,
                llm::ToolClient,
            };

            let mut client = match ToolClient::new(config) {
                Ok(c) => c,
                Err(e) => {
                    let _ = chunk_tx2.send(format!("**Error creating LLM client: {}**\n", e));
                    let _ = chunk_tx2.send("__DONE__".into());
                    return;
                }
            };

            let mut handler = GenericAgentHandler::new(&query, &work_dir2);
            let system_prompt = build_system_prompt(&work_dir2);
            let tools = full_tools_schema();

            let _ = agent_runner_loop(
                &mut client,
                &system_prompt,
                &query,
                &mut handler,
                &tools,
                50,
                false,
                &chunk_tx2,
            )
            .await;

            let _ = chunk_tx2.send("__DONE__".into());
        });

        tokio::select! {
            _ = async {
                while let Some(chunk) = chunk_rx.recv().await {
                    if chunk == "__DONE__" {
                        let _ = app2.emit("agent-done", ());
                        let mut s = shared2.lock().await;
                        s.is_running = false;
                        s.abort_tx = None;
                        break;
                    }
                    let _ = app2.emit("agent-chunk", chunk);
                }
            } => {}
            _ = abort_rx => {
                agent_handle.abort();
                let _ = app2.emit("agent-chunk", "\n**[任务已中止]**\n".to_string());
                let _ = app2.emit("agent-done", ());
                let mut s = shared2.lock().await;
                s.is_running = false;
                s.abort_tx = None;
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn abort_task(state: State<'_, AppState>) -> Result<(), String> {
    let mut s = state.0.lock().await;
    if let Some(tx) = s.abort_tx.take() {
        let _ = tx.send(());
    }
    Ok(())
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<StatusPayload, String> {
    let s = state.0.lock().await;
    let llm_name = s.llm_names.get(s.llm_index).cloned().unwrap_or_default();
    Ok(StatusPayload {
        is_running: s.is_running,
        llm_name,
        current_turn: s.current_turn,
    })
}

#[tauri::command]
async fn switch_llm(state: State<'_, AppState>) -> Result<String, String> {
    let mut s = state.0.lock().await;
    if s.llm_names.len() > 1 {
        s.llm_index = (s.llm_index + 1) % s.llm_names.len();
    }
    Ok(s.llm_names.get(s.llm_index).cloned().unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn load_config() -> AppConfig {
    let home = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));

    let candidates = [
        std::path::PathBuf::from("mykey.json"),
        std::path::PathBuf::from("config.json"),
        home.join(".config/pc-agent/mykey.json"),
    ];
    for p in &candidates {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&text) {
                return cfg;
            }
        }
    }
    AppConfig { oai_config: None, claude_config: None, proxy: None }
}

fn detect_llm_names(config: &AppConfig) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(oai) = &config.oai_config {
        names.push(format!("OAI/{}", oai.model));
    }
    if let Some(claude) = &config.claude_config {
        names.push(format!("Claude/{}", claude.model));
    }
    if names.is_empty() {
        names.push("(no backend configured)".into());
    }
    names
}

fn init_work_dir(work_dir: &str) {
    let base = std::path::Path::new(work_dir);
    let _ = std::fs::create_dir_all(base.join("memory"));
    let _ = std::fs::create_dir_all(base.join("temp"));
    let insight = base.join("memory").join("global_mem_insight.txt");
    if !insight.exists() {
        let _ = std::fs::write(&insight, "");
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let config = load_config();
    let llm_names = detect_llm_names(&config);
    let work_dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".into());
    init_work_dir(&work_dir);

    let state = AppState(Arc::new(Mutex::new(Inner {
        config,
        work_dir,
        is_running: false,
        current_turn: 0,
        llm_names,
        llm_index: 0,
        abort_tx: None,
    })));

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            run_task,
            abort_task,
            get_status,
            switch_llm,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
