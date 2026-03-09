# pc-agent-loop-rs

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![平台](https://img.shields.io/badge/平台-Linux%20%7C%20macOS%20%7C%20Windows%20%7C%20Android-blue)](#平台支持)

[pc-agent-loop](https://github.com/lsdefine/pc-agent-loop) 的纯 Rust 移植版。极简自主 Agent 框架，赋予任意 LLM 对 PC 的物理级控制能力：浏览器、终端、文件系统等。编译为单一二进制文件，无需 Python 或其他运行时。

> **原版 Python**：~3,300 行
> **本移植版**：~2,500 行异步 Rust，Cargo 工作区，多平台 FFI

## 工作原理

```
用户指令
    ↓
┌──────────────────────────┐
│  agent_loop（核心）       │  ← 感知-思考-行动循环
│  文本协议 LLM 调用        │     <thinking> → <tool_use> → tool_result
└──────────┬────────────────┘
           ↓
┌──────────────────────────┐
│  8 个原子工具             │  ← 全部能力来源
│  code_run                │     执行 Python / Bash / Lua（内嵌）/ JS
│  file_read               │     按行范围读取 + 关键字搜索
│  file_write              │     新建 / 覆盖 / 追加
│  file_patch              │     基于唯一字符串匹配的精细修改
│  web_scan                │     获取浏览器实时页面 HTML
│  web_execute_js          │     在真实浏览器中执行 JS
│  ask_user                │     人在回路中断点
│  update_working_checkpoint │   短期工作记忆
└──────────┬────────────────┘
           ↓
┌──────────────────────────┐
│  记忆系统                 │  ← 跨会话持久化
│  key_info 便签            │     每轮工作记忆
│  history <summary>        │     每轮摘要提取
│  global_mem_insight.txt   │     长期事实与学到的 SOP
└──────────────────────────┘
```

Agent 使用**文本协议**工具调用（非原生 function calling），因此可用于任何能生成文本的 LLM——OpenAI、Claude、Gemini 或通过 OpenAI 兼容端点的本地模型。

## 快速开始

```bash
# 1. 克隆
git clone https://github.com/kingfree/pc-agent-loop-rs.git
cd pc-agent-loop-rs

# 2. 配置 API Key
cat > mykey.json << 'EOF'
{
  "oai_config": {
    "apikey": "sk-...",
    "apibase": "https://api.openai.com",
    "model": "gpt-4o"
  }
}
EOF

# 3. 编译并运行（交互式 CLI）
cargo run --release -p pc-agent-loop

# 4. 或启动 Tauri 桌面 GUI
cargo run --release -p pc-agent-loop-gui
```

## 能做什么

```
你：「监控股价并提醒我」
Agent：安装依赖 → 构建筛选流程 → 设置定时任务 → 保存为 SOP
下次：一句话复现

你：「读取网页并总结」
Agent：web_scan → 提取内容 → 总结 → 完成

你：「修复 main.rs 中的 bug」
Agent：file_read → 分析 → file_patch → 验证
```

每个解决过的任务都可以成为存储在 `memory/` 中的永久 SOP。用几周后，你的实例就拥有了从使用中生长出的独特技能树。

## 体积与运行时对比

| 指标 | Python 原版 | Rust 移植版 |
|---|---|---|
| 发行体积 | Python 3 ~50 MB + pip 包 ~30–200 MB | 单一二进制 **~8 MB**（stripped） |
| 启动耗时 | 1–3 秒（Python 导入） | < 50 ms |
| 空闲内存 | ~60–120 MB | ~10–20 MB |
| 运行时依赖 | Python 3 + pip | **无** |
| Lua 解释器 | 需要外部 `lua` 命令 | **内嵌** Lua 5.4（mlua 编译进二进制） |
| Lua 沙箱 | 无隔离 | 独立阻塞线程 + `ALL_SAFE` stdlib（禁止 `io`/`os`/`require`/`ffi`） |

> 测量环境：Linux x86_64（Ubuntu 22.04）。Python 体积含典型 venv（requests、anthropic、websocket-client）。

## 出厂清单

**核心引擎**（`crates/core`）：
- `agent_loop` — 感知-思考-行动循环，`UnboundedSender<String>` 流式输出
- `llm` — 多后端 LLM 客户端（OpenAI、Claude、Gemini），SSE 流式，自动重试
- `handler` — 工具分发 + 工作记忆 + `<summary>` 提取
- `tools` — `code_run`（Python / Bash / **Lua 内嵌** / JavaScript）、`file_read`、`file_patch`、`file_write`
- `webdriver` — 通过 Tampermonkey 注入真实浏览器的 WebSocket+HTTP 桥接

**交互界面**：
- `crates/cli` — 桌面 CLI，支持全部运行模式
- `crates/gui` — Tauri 原生桌面应用（WebView + 暗色主题）
- `crates/android` — Android JNI cdylib（`libpc_agent_loop_android.so`）
- `crates/ios` — iOS C FFI staticlib（`libpc_agent_loop_ios.a`）

**5 个核心 SOP**（`memory/` 目录，与 Python 原版格式完全相同）：
1. `memory_management_sop` — Agent 如何管理自身记忆
2. `autonomous_operation_sop` — 自主任务执行
3. `scheduled_task_sop` — 定时任务
4. `web_setup_sop` — 浏览器环境引导
5. `ljqCtrl_sop` — 桌面物理控制

## 平台支持

| 平台 | 产物 | 构建目标 |
|------|------|---------|
| Linux x86_64 | CLI + GUI（.AppImage/.deb） | `x86_64-unknown-linux-gnu` |
| Linux aarch64 | CLI | `aarch64-unknown-linux-gnu` |
| macOS Apple Silicon | CLI + GUI（.dmg） | `aarch64-apple-darwin` |
| macOS Intel | CLI + GUI | `x86_64-apple-darwin` |
| Windows x86_64 | CLI + GUI（.msi） | `x86_64-pc-windows-msvc` |
| Android | JNI `.so`（arm64-v8a + x86_64） | `aarch64-linux-android` |
| iOS | Static `.a` | `aarch64-apple-ios` |

Windows 子进程自动使用 `CREATE_NO_WINDOW` 抑制控制台窗口。

## CLI 用法

```bash
# 交互式（从 stdin 读取）
pc-agent-loop

# 单次任务
pc-agent-loop --task "列出所有 Rust 文件并统计行数"

# 文件 IO 模式（读 input.txt，写 output.txt）
pc-agent-loop task ./my-task-dir/

# 定时任务模式——每 ~60 秒轮询 sche_tasks/pending/
pc-agent-loop scheduled

# 反射模式——定期运行 check() 脚本，将返回值作为任务
pc-agent-loop --reflect reflect/autonomous.py

# 带浏览器自动化
pc-agent-loop --webdriver --task "打开 example.com 并获取标题"

# 仅启动 WebDriver 服务器
pc-agent-loop webdriver --port 9999

# 选择 LLM 后端（多后端配置时，0 起始索引）
pc-agent-loop --llm-no 1

# 详细输出（逐 token 流式）
pc-agent-loop --verbose --task "..."

# 限制 Agent 轮数
pc-agent-loop --max-turns 30 --task "..."
```

## Tauri 桌面 GUI

```bash
# 开发模式启动
cargo run --release -p pc-agent-loop-gui
```

功能：
- **流式聊天** — Agent 输出逐 token 通过 IPC 事件传输
- **Markdown 渲染** — 粗体、内联代码、代码块
- **LLM 切换** — 在已配置的后端之间循环
- **中止任务** — 随时停止当前执行
- **轮数显示** — 侧边栏显示当前 Agent 轮次

## 配置（mykey.json）

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

配置查找顺序：`--config` 参数 → `./mykey.json` → `~/.config/pc-agent/mykey.json`。

多后端可同时配置。使用 `--llm-no`（CLI）或 GUI 的「切换 LLM」按钮切换。

## 浏览器自动化

`web_scan` 和 `web_execute_js` 通过 TMWebDriver 服务器（默认 `localhost:18766`）连接运行中的浏览器：

1. 安装 [Tampermonkey](https://www.tampermonkey.net/) 浏览器扩展
2. 从[原始项目](https://github.com/lsdefine/pc-agent-loop)的 `assets/tmwd_cdp_bridge/` 安装用户脚本
3. 启动 WebDriver 服务器：

```bash
# 随 Agent 一起启动
pc-agent-loop --webdriver --task "总结 HN 热门文章"

# 独立服务器
pc-agent-loop webdriver --port 9999
```

该桥接注入你的真实浏览器——无沙箱，保留登录状态，适用于任何网站。

## 定时任务

将任务文件放入 `sche_tasks/pending/`，文件名格式：`YYYY-MM-DD_HHMM_描述.txt`：

```
sche_tasks/pending/2025-03-10_0900_morning-briefing.txt
```

启动调度器（每 ~60 秒轮询）：

```bash
pc-agent-loop scheduled
```

任务自动经历 `pending/` → `running/` → `done/` 流转。

## 反射模式

反射模式以固定间隔运行 Python 检查脚本。当 `check()` 返回字符串时，作为任务提交：

```bash
pc-agent-loop --reflect reflect/autonomous.py   # 每 30 分钟自动化
pc-agent-loop --reflect reflect/scheduler.py    # 定时任务触发器
```

自定义示例：

```python
INTERVAL = 300  # 检查间隔（秒）
ONCE = False    # True 表示首次触发后退出

def check():
    if some_condition():
        return "因为 Y 发生，执行 X"
    return None
```

## Android 集成

```bash
# 构建
cargo install cargo-ndk
cargo ndk -t arm64-v8a build --release -p pc-agent-loop-android
# → target/aarch64-linux-android/release/libpc_agent_loop_android.so
```

将 `.so` 复制到 Android 项目的 `app/src/main/jniLibs/arm64-v8a/`，然后：

```kotlin
package com.pcagentloop

class AgentSession(configJson: String, workDir: String) {
    private val ptr: Long = nativeCreate(configJson, workDir).also {
        if (it == 0L) throw RuntimeException("创建 AgentSession 失败")
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

## iOS 集成

```bash
# 构建
rustup target add aarch64-apple-ios
cargo build --target aarch64-apple-ios --release -p pc-agent-loop-ios
# → target/aarch64-apple-ios/release/libpc_agent_loop_ios.a
```

在 Xcode 中链接 `libpc_agent_loop_ios.a`，然后使用 Swift 封装：

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

完整 C 头文件见 `crates/ios/src/lib.rs`。

## 与 Python 原版对比

| | Python 原版 | Rust 移植版 |
|---|---|---|
| 代码量 | ~3,300 行 | ~2,500 行 |
| 运行时 | Python 3 + pip | 单一二进制文件 |
| GUI | Streamlit + pywebview | Tauri（原生 WebView） |
| Telegram Bot | tgapp.py | 暂未移植 |
| 浏览器桥接 | bottle + simple_websocket_server | axum + tokio-tungstenite |
| HTML 简化器 | BeautifulSoup + 自定义 JS | TMWebDriver HTTP 中继 |
| LLM 后端 | OpenAI、Claude、Gemini、xAI、Sider | OpenAI、Claude、Gemini |
| 移动端 | Termux（Python CLI） | 原生 JNI / C FFI |
| 记忆格式 | `memory/` 中的文件 | 同格式同目录 |
| SOP | 5 个核心，自动增长 | 5 个核心（相同 `.md` 文件） |

## 构建

```bash
# 检查所有 crate
cargo check --workspace

# 桌面发布版
cargo build --release -p pc-agent-loop
cargo build --release -p pc-agent-loop-gui

# Android（需要 Android NDK + cargo-ndk）
cargo install cargo-ndk
cargo ndk -t arm64-v8a -t x86_64 build --release -p pc-agent-loop-android

# iOS（需要 macOS + Xcode）
rustup target add aarch64-apple-ios x86_64-apple-ios
cargo build --target aarch64-apple-ios --release -p pc-agent-loop-ios
```

## 许可

MIT — 见 [LICENSE](LICENSE)
