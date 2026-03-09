# pc-agent-loop-rs

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows%20%7C%20Android%20%7C%20iOS-blue)](#platform-support)

[English](#english) | [中文](#chinese)

<a name="english"></a>

A pure-Rust port of [pc-agent-loop](https://github.com/lsdefine/pc-agent-loop) — a minimalist autonomous agent framework that gives any LLM physical-level control over your PC: browser, terminal, file system, and beyond. Compiles to a single binary with zero runtime dependencies.

> **Original project**: ~3,300 lines of Python  
> **This port**: ~2,500 lines of async Rust, Cargo workspace with multi-platform FFI

## What It Does

```
You: "Monitor stock prices and alert me"
Agent: installs dependencies → builds screening workflow → sets up scheduled task → saves as SOP
Next time: one sentence to run.

You: "Read the webpage and summarize it"
Agent: web_scan → extract content → summarize → done

You: "Fix the bug in main.rs"
Agent: file_read → analyze → file_patch → verify
```

Every task the agent solves can become a permanent SOP stored in `memory/`. After a few weeks, your instance has a unique skill tree grown from use.

## Quick Start

```bash
# 1. Clone
git clone https://github.com/kingfree/pc-agent-loop-rs.git
cd pc-agent-loop-rs

# 2. Configure API key
cat > mykey.json << 'EOF'
{
  "oai_config": {
    "apikey": "sk-...",
    "apibase": "https://api.openai.com",
    "model": "gpt-4o"
  }
}
EOF

# 3. Build and run (interactive CLI)
cargo run --release -p pc-agent-loop

# 4. Or launch the Web UI
cargo run --release -p pc-agent-loop-gui -- --open
```

The Web UI opens at `http://localhost:7891` — a dark-themed chat interface with streaming output, LLM switching, and abort controls.

**Also runs on Android** — the `pc-agent-loop-android` crate compiles to a JNI `.so` usable from Kotlin/Java. See [Android Integration](#android-integration).

## How It Works

```
User instruction
      ↓
┌──────────────────────────┐
│  agent_loop (core)        │  ← Sense-Think-Act cycle
│  text-protocol LLM call   │     <thinking> → <tool_use> → tool_result
└──────────┬────────────────┘
           ↓
┌──────────────────────────┐
│  7 Atomic Tools           │  ← All capabilities derive from these
│  code_run                 │     Execute Python / Bash / PowerShell
│  file_read                │     Read with line ranges & keyword search
│  file_write               │     Overwrite / append / prepend
│  file_patch               │     Surgical unique-match edits
│  web_scan                 │     Get live page HTML via browser bridge
│  web_execute_js           │     Execute JS in real browser
│  ask_user                 │     Human-in-the-loop breakpoint
└──────────┬────────────────┘
           ↓
┌──────────────────────────┐
│  Memory System            │  ← Persistent across sessions
│  key_info checkpoint      │     Short-term working memory per turn
│  history summaries        │     Per-turn <summary> extraction
│  global_mem_insight.txt   │     Long-term facts & learned SOPs
└──────────────────────────┘
```

The agent uses a **text-based tool protocol** (not native function calling), so it works with any LLM that can generate text — OpenAI, Claude, Gemini, or local models via an OpenAI-compatible endpoint.

## What Ships in the Box

**Core engine** (`crates/core`):
- `agent_loop` — Sense-Think-Act loop + `UnboundedSender<String>` streaming
- `llm` — Multi-backend LLM client (OpenAI, Claude, Gemini), SSE streaming, auto-retry
- `handler` — Tool dispatch + working-memory management + `<summary>` extraction
- `tools` — `code_run`, `file_read`, `file_patch`, `file_write`
- `webdriver` — WebSocket + HTTP bridge to a real browser via Tampermonkey

**Interfaces**:
- `crates/cli` — Desktop CLI (`pc-agent-loop` binary), all run modes
- `crates/gui` — Web chat UI (axum + SSE, dark theme, Markdown rendering)
- `crates/android` — Android JNI cdylib (`libpc_agent_loop_android.so`)
- `crates/ios` — iOS C FFI staticlib (`libpc_agent_loop_ios.a`)

**Core SOPs** (ship in `memory/`, version-controlled — same `.md` files as the Python original):
1. `memory_management_sop` — How the agent manages its own memory
2. `autonomous_operation_sop` — Self-directed task execution
3. `scheduled_task_sop` — Cron-like recurring tasks
4. `web_setup_sop` — Browser environment bootstrap
5. `ljqCtrl_sop` — Desktop physical control (keyboard, mouse)

## Platform Support

| Platform | Binary / Library | Build Target |
|----------|-----------------|--------------|
| Linux    | Native CLI + GUI | `x86_64-unknown-linux-gnu` |
| macOS    | Native CLI + GUI | `aarch64-apple-darwin` |
| Windows  | Native CLI + GUI | `x86_64-pc-windows-msvc` |
| Android  | JNI `.so`        | `aarch64-linux-android` |
| iOS      | Static `.a`      | `aarch64-apple-ios` |

Windows subprocesses automatically use `CREATE_NO_WINDOW` to suppress console windows.

## CLI Usage

```bash
# Interactive mode (read from stdin)
pc-agent-loop

# Single task
pc-agent-loop --task "List all Rust files and count lines"

# Task from IO directory (reads input.txt, writes output.txt)
pc-agent-loop task ./my-task-dir/

# Scheduled mode — polls sche_tasks/pending/ every ~60s
pc-agent-loop scheduled

# Reflect mode — runs a check() script on interval, submits result as task
pc-agent-loop --reflect reflect/autonomous.py

# With WebDriver browser automation server
pc-agent-loop --webdriver --task "Open example.com and get the title"

# Start WebDriver server only
pc-agent-loop webdriver --port 9999

# Select LLM backend (0-indexed, when multiple configured)
pc-agent-loop --llm-no 1

# Verbose (stream raw LLM output token by token)
pc-agent-loop --verbose --task "..."

# Limit agent turns
pc-agent-loop --max-turns 30 --task "..."
```

## Web GUI

```bash
# Start Web UI on default port 7891
cargo run --release -p pc-agent-loop-gui

# Custom port and auto-open browser
cargo run --release -p pc-agent-loop-gui -- --port 8080 --open

# Custom work directory
cargo run --release -p pc-agent-loop-gui -- --work-dir /home/user/agent-work
```

The GUI provides:
- **Streaming chat** — agent output streamed token by token via SSE
- **Markdown rendering** — bold, inline code, fenced code blocks
- **LLM switcher** — cycle through configured backends
- **Abort** — stop the current task mid-execution
- **Turn counter** — shows current agent turn in the sidebar

## Configuration (mykey.json)

```json
{
  "oai_config": {
    "apikey": "sk-...",
    "apibase": "https://api.openai.com",
    "model": "gpt-4o"
  },
  "claude_config": {
    "apikey": "sk-ant-...",
    "model": "claude-opus-4-5"
  },
  "gemini_config": {
    "apikey": "AIza...",
    "model": "gemini-2.0-flash"
  },
  "proxy": "http://127.0.0.1:7890"
}
```

Config is searched in order: `--config` flag → `./mykey.json` → `~/.config/pc-agent/mykey.json`.

Multiple backends can be configured simultaneously. Use `--llm-no` (CLI) or the GUI's "切换 LLM" button to switch.

## Browser Automation

The `web_scan` and `web_execute_js` tools connect to a running TMWebDriver server (default `localhost:18766`). To use them:

1. Install the [Tampermonkey](https://www.tampermonkey.net/) browser extension
2. Install the userscript from `assets/tmwd_cdp_bridge/` of the [original project](https://github.com/lsdefine/pc-agent-loop)
3. Start the WebDriver server:

```bash
# Alongside the agent
pc-agent-loop --webdriver --task "Summarize the top HN posts"

# Standalone server
pc-agent-loop webdriver --port 9999
```

The bridge injects into your real browser — no sandboxing, keeps login state, works with any site.

## Scheduled Tasks

Drop task files into `sche_tasks/pending/` with filename `YYYY-MM-DD_HHMM_description.txt`:

```
sche_tasks/pending/2025-03-10_0900_morning-briefing.txt
```

Start the scheduler (polls every ~60s):

```bash
pc-agent-loop scheduled
```

Tasks move through `pending/` → `running/` → `done/` automatically.

## Reflect Mode

Reflect mode runs a Python check script on a fixed interval. When `check()` returns a string, it's submitted as a task:

```bash
pc-agent-loop --reflect reflect/autonomous.py   # idle automation every 30min
pc-agent-loop --reflect reflect/scheduler.py    # scheduled task trigger
```

Write your own:

```python
INTERVAL = 300  # seconds between checks
ONCE = False    # True to exit after first trigger

def check():
    if some_condition():
        return "Do X because Y happened"
    return None
```

## Android Integration

```bash
# Build
cargo install cargo-ndk
cargo ndk -t arm64-v8a build --release -p pc-agent-loop-android
# → target/aarch64-linux-android/release/libpc_agent_loop_android.so
```

Copy the `.so` into your Android project's `app/src/main/jniLibs/arm64-v8a/`, then:

```kotlin
package com.pcagentloop

class AgentSession(configJson: String, workDir: String) {
    private val ptr: Long = nativeCreate(configJson, workDir).also {
        if (it == 0L) throw RuntimeException("Failed to create AgentSession")
    }

    suspend fun runTask(task: String, maxTurns: Int = 15): String =
        withContext(Dispatchers.IO) { nativeRunTask(ptr, task, maxTurns) }

    fun close() = nativeDestroy(ptr)

    private external fun nativeCreate(config: String, workDir: String): Long
    private external fun nativeRunTask(ptr: Long, task: String, maxTurns: Int): String
    private external fun nativeDestroy(ptr: Long)

    companion object {
        init { System.loadLibrary("pc_agent_loop_android") }
    }
}
```

## iOS Integration

```bash
# Build
rustup target add aarch64-apple-ios
cargo build --target aarch64-apple-ios --release -p pc-agent-loop-ios
# → target/aarch64-apple-ios/release/libpc_agent_loop_ios.a
```

Link `libpc_agent_loop_ios.a` in Xcode, then use the Swift wrapper:

```swift
class AgentSession {
    private var ptr: OpaquePointer?

    init(configJson: String, workDir: String) throws {
        ptr = agent_session_create(configJson, workDir)
        guard ptr != nil else { throw AgentError.initFailed }
    }

    func runTask(_ task: String, maxTurns: Int32 = 15) -> String {
        let result = agent_session_run_task(ptr, task, maxTurns)!
        defer { agent_string_free(result) }
        return String(cString: result)
    }

    deinit { agent_session_destroy(ptr) }
}
```

The full C header is documented in `crates/ios/src/lib.rs`.

## vs. Python Original

| | pc-agent-loop (Python) | pc-agent-loop-rs (Rust) |
|---|---|---|
| Lines | ~3,300 | ~2,500 |
| Runtime | Python 3 + pip deps | Single binary, no runtime |
| GUI | Streamlit + pywebview | axum + embedded HTML/JS |
| Telegram bot | tgapp.py | Not yet ported |
| Browser bridge | bottle + simple_websocket_server | axum + tokio-tungstenite |
| HTML simplifier | BeautifulSoup + custom JS | TMWebDriver HTTP relay |
| LLM backends | OpenAI, Claude, Gemini, xAI, Sider | OpenAI, Claude, Gemini |
| Mobile | Termux (Python CLI) | Native JNI / C FFI |
| Memory format | Files in `memory/` | Same files, same format |
| SOPs | 5 core, self-growing | 5 core (same `.md` files) |

## Building

```bash
# Check all crates
cargo check --workspace

# Desktop release
cargo build --release -p pc-agent-loop
cargo build --release -p pc-agent-loop-gui

# Android (requires Android NDK + cargo-ndk)
cargo install cargo-ndk
cargo ndk -t arm64-v8a -t x86_64 build --release -p pc-agent-loop-android

# iOS (requires Xcode on macOS)
rustup target add aarch64-apple-ios x86_64-apple-ios
cargo build --target aarch64-apple-ios --release -p pc-agent-loop-ios
```

## License

MIT — see [LICENSE](LICENSE)

---

<a name="chinese"></a>

# pc-agent-loop-rs（中文说明）

[pc-agent-loop](https://github.com/lsdefine/pc-agent-loop) 的纯 Rust 移植版。极简自主 Agent 框架，让任意 LLM 获得 PC 物理级控制能力。编译为单个二进制文件，无需 Python 或其他运行时。

## 快速开始

```bash
git clone https://github.com/kingfree/pc-agent-loop-rs.git
cd pc-agent-loop-rs

# 配置 API Key
echo '{"oai_config":{"apikey":"sk-...","apibase":"https://api.openai.com","model":"gpt-4o"}}' > mykey.json

# 交互式 CLI
cargo run --release -p pc-agent-loop

# Web 界面（自动打开浏览器）
cargo run --release -p pc-agent-loop-gui -- --open
```

## 出厂清单

**核心引擎**（`crates/core`）：
- `agent_loop` — 感知-思考-行动循环，流式输出通道
- `llm` — 多后端 LLM（OpenAI、Claude、Gemini），SSE 流式，自动重试
- `handler` — 工具分发 + 工作记忆 + `<summary>` 提取
- `tools` — `code_run`、`file_read`、`file_patch`、`file_write`
- `webdriver` — 通过 Tampermonkey 注入真实浏览器的 WebSocket+HTTP 桥接

**交互界面**：
- `crates/cli` — 桌面 CLI，支持全部运行模式
- `crates/gui` — Web 聊天界面（axum + SSE，暗色主题，Markdown 渲染）
- `crates/android` — Android JNI cdylib
- `crates/ios` — iOS C FFI staticlib

**5 个核心 SOP**（`memory/` 目录，与 Python 原版格式完全相同）：
1. `memory_management_sop` — Agent 如何管理自身记忆
2. `autonomous_operation_sop` — 自主任务执行
3. `scheduled_task_sop` — 定时任务
4. `web_setup_sop` — 浏览器环境引导
5. `ljqCtrl_sop` — 桌面物理控制

## 配置（mykey.json）

```json
{
  "oai_config": {"apikey": "sk-...", "apibase": "https://api.openai.com", "model": "gpt-4o"},
  "claude_config": {"apikey": "sk-ant-...", "model": "claude-opus-4-5"},
  "gemini_config": {"apikey": "AIza...", "model": "gemini-2.0-flash"},
  "proxy": "http://127.0.0.1:7890"
}
```

## CLI 模式

```bash
pc-agent-loop                              # 交互式
pc-agent-loop --task "..."                 # 单次任务
pc-agent-loop task ./my-task-dir/          # 文件 IO 模式
pc-agent-loop scheduled                    # 定时任务轮询
pc-agent-loop --reflect reflect/autonomous.py  # 反射模式
pc-agent-loop --webdriver --task "..."     # 带浏览器自动化
pc-agent-loop --llm-no 1                   # 选择 LLM 后端
```

## Web 界面

```bash
cargo run --release -p pc-agent-loop-gui -- --open
```

功能：流式聊天输出、Markdown 渲染、LLM 切换、中止任务、当前轮数显示。

## 与 Python 原版对比

| | Python 原版 | Rust 移植版 |
|---|---|---|
| 代码量 | ~3,300 行 | ~2,500 行 |
| 运行时 | Python 3 + pip | 单一二进制文件 |
| GUI | Streamlit + pywebview | axum + 内嵌 HTML/JS |
| 移动端 | Termux CLI | 原生 JNI / C FFI |
| LLM 后端 | OpenAI、Claude、Gemini、xAI、Sider | OpenAI、Claude、Gemini |
| 记忆文件 | `memory/*.md` | 同格式 `memory/*.md` |

## 许可

MIT — 见 [LICENSE](LICENSE)
