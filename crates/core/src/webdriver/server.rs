use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// A pending JS execution request
#[allow(dead_code)]
#[derive(Debug)]
pub struct PendingRequest {
    pub id: String,
    pub js: String,
    pub tx: tokio::sync::oneshot::Sender<Value>,
}

/// Shared state for the WebDriver server
#[derive(Debug)]
pub struct WebDriverState {
    /// Sessions: session_id -> session info
    pub sessions: RwLock<HashMap<String, SessionInfo>>,
    /// Pending JS requests waiting for the browser to execute
    pub pending_requests: Mutex<Vec<(String, Value)>>, // (id, {js})
    /// Results from browser (kept for debugging/logging)
    #[allow(dead_code)]
    pub results: Mutex<HashMap<String, Value>>,
    /// Result notification channels
    pub result_channels: Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub created_at: String,
    pub last_seen: String,
    pub url: String,
}

impl WebDriverState {
    pub fn new() -> Arc<Self> {
        Arc::new(WebDriverState {
            sessions: RwLock::new(HashMap::new()),
            pending_requests: Mutex::new(Vec::new()),
            results: Mutex::new(HashMap::new()),
            result_channels: Mutex::new(HashMap::new()),
        })
    }
}

type SharedState = Arc<WebDriverState>;

/// TMWebDriver: WebSocket + HTTP server for browser automation.
/// Mirrors Python's TMWebDriver class.
pub struct TMWebDriver {
    pub addr: SocketAddr,
    pub state: SharedState,
}

impl TMWebDriver {
    pub fn new(port: u16) -> Self {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        TMWebDriver {
            addr,
            state: WebDriverState::new(),
        }
    }

    /// Start the server in the background
    pub async fn start(self: Arc<Self>) -> Result<()> {
        let state = self.state.clone();
        let addr = self.addr;

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .route("/session", post(create_session))
            .route("/session/:id", get(get_session))
            .route("/execute", post(execute_js))
            .route("/poll", get(poll_requests))
            .route("/result", post(submit_result))
            .route("/health", get(health_check))
            .with_state(state);

        info!("TMWebDriver server starting on {}", addr);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        Ok(())
    }

    /// Execute JavaScript in the browser (called from agent side)
    pub async fn execute_js(&self, js: &str, timeout_secs: u64) -> Result<Value> {
        let req_id = Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut pending = self.state.pending_requests.lock().await;
            pending.push((req_id.clone(), json!({ "id": req_id, "js": js })));
        }
        {
            let mut channels = self.state.result_channels.lock().await;
            channels.insert(req_id.clone(), tx);
        }

        // Wait for result with timeout
        match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(anyhow::anyhow!("Result channel closed")),
            Err(_) => {
                // Clean up
                let mut channels = self.state.result_channels.lock().await;
                channels.remove(&req_id);
                Err(anyhow::anyhow!("JS execution timed out after {}s", timeout_secs))
            }
        }
    }
}

// HTTP Handlers

async fn health_check() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn create_session(
    State(state): State<SharedState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let session_id = Uuid::new_v4().to_string();
    let now = chrono::Local::now().to_rfc3339();
    let url = body.get("url").and_then(|u| u.as_str()).unwrap_or("about:blank").to_string();

    let session = SessionInfo {
        session_id: session_id.clone(),
        created_at: now.clone(),
        last_seen: now,
        url,
    };

    let mut sessions = state.sessions.write().await;
    sessions.insert(session_id.clone(), session.clone());
    info!("Created session: {}", session_id);

    Json(json!({
        "session_id": session_id,
        "status": "created"
    }))
}

async fn get_session(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    if let Some(session) = sessions.get(&id) {
        Json(json!(session))
    } else {
        Json(json!({ "error": "session not found" }))
    }
}

/// Agent submits a JS execution request
async fn execute_js(
    State(state): State<SharedState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let req_id = Uuid::new_v4().to_string();
    let js = body.get("js")
        .or_else(|| body.get("script"))
        .and_then(|j| j.as_str())
        .unwrap_or("")
        .to_string();

    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();

    {
        let mut pending = state.pending_requests.lock().await;
        pending.push((req_id.clone(), json!({ "id": req_id, "js": js })));
    }
    {
        let mut channels = state.result_channels.lock().await;
        channels.insert(req_id.clone(), tx);
    }

    // Long-poll for result (up to 30 seconds)
    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(result)) => Json(json!({ "id": req_id, "result": result, "status": "ok" })),
        Ok(Err(_)) => Json(json!({ "id": req_id, "error": "channel closed", "status": "error" })),
        Err(_) => Json(json!({ "id": req_id, "error": "timeout", "status": "timeout" })),
    }
}

/// Browser polls for pending requests (long-polling)
async fn poll_requests(
    State(state): State<SharedState>,
) -> impl IntoResponse {
    // Return pending requests (and clear them)
    let mut pending = state.pending_requests.lock().await;
    let requests: Vec<Value> = pending.drain(..).map(|(_, req)| req).collect();
    Json(json!({ "requests": requests }))
}

/// Browser submits a result
async fn submit_result(
    State(state): State<SharedState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let req_id = body.get("id")
        .and_then(|id| id.as_str())
        .unwrap_or("");
    let result = body.get("result").cloned().unwrap_or(Value::Null);

    let mut channels = state.result_channels.lock().await;
    if let Some(tx) = channels.remove(req_id) {
        let _ = tx.send(result.clone());
        debug!("Sent result for request {}", req_id);
        Json(json!({ "status": "ok" }))
    } else {
        warn!("No pending channel for request {}", req_id);
        Json(json!({ "status": "not_found" }))
    }
}

/// WebSocket handler for real-time browser communication
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_websocket(socket, state))
}

async fn handle_websocket(socket: WebSocket, state: SharedState) {
    let (mut sender, mut receiver) = socket.split();
    let session_id = Uuid::new_v4().to_string();
    info!("WebSocket connected: {}", session_id);

    // Send pending requests to the browser
    let state_clone = state.clone();
    let session_id_clone = session_id.clone();

    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            let mut pending = state_clone.pending_requests.lock().await;
            for (_, req) in pending.drain(..) {
                let msg = serde_json::to_string(&req).unwrap_or_default();
                if sender.send(Message::Text(msg.into())).await.is_err() {
                    debug!("WebSocket send error for session {}", session_id_clone);
                    return;
                }
            }
        }
    });

    // Receive results from browser
    let mut recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(data) = serde_json::from_str::<Value>(&text) {
                        let req_id = data.get("id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("")
                            .to_string();
                        let result = data.get("result").cloned().unwrap_or(Value::Null);

                        let mut channels = state.result_channels.lock().await;
                        if let Some(tx) = channels.remove(&req_id) {
                            let _ = tx.send(result);
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket closed for session {}", session_id);
                    break;
                }
                Ok(Message::Ping(_)) => {
                    // Pong is handled automatically by axum
                    debug!("Ping received");
                }
                Err(e) => {
                    warn!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
    });

    // Wait for either task to finish
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}
