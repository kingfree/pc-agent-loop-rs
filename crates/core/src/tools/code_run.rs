use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

/// Execute code as a subprocess, streaming output.
/// Mirrors Python's `code_run` function in ga.py, extended with Lua and JS support.
///
/// Args:
/// - `type` / `language`: "python" | "bash" | "powershell" | "lua" | "javascript" / "js" / "node"
///   (default: "python")
/// - `code`: code string to execute
/// - `timeout`: timeout in seconds (default: 60)
/// - `cwd`: working directory (default: current directory)
///
/// For Python/Lua/JS, writes to a temp file and runs it (proper multiline support).
/// For bash/powershell, runs inline via shell.
pub async fn code_run(args: &Value, tx: &UnboundedSender<String>) -> Result<(String, i32)> {
    // Support both "type" (Python original) and "language" (Rust alias)
    let code_type = args
        .get("type")
        .or_else(|| args.get("language"))
        .and_then(|l| l.as_str())
        .unwrap_or("python");

    let code = args
        .get("code")
        .or_else(|| args.get("script"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let timeout_secs = args.get("timeout").and_then(|t| t.as_u64()).unwrap_or(60);

    let cwd = args.get("cwd").and_then(|c| c.as_str());

    debug!(
        "code_run: type={}, code_len={}, cwd={:?}",
        code_type,
        code.len(),
        cwd
    );

    if code.is_empty() {
        return Ok(("No code provided".to_string(), 1));
    }

    // Preview for logging
    let preview = {
        let s: String = code.chars().take(60).collect::<String>().replace('\n', " ");
        if code.len() > 60 {
            format!("{}...", s)
        } else {
            s.trim().to_string()
        }
    };
    let dir_name = cwd
        .map(|d| {
            Path::new(d)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(d)
        })
        .unwrap_or(".");
    let _ = tx.send(format!(
        "[Action] Running {} in {}: {}\n",
        code_type, dir_name, preview
    ));

    match code_type {
        "python" | "python3" | "py" => run_python(code, timeout_secs, cwd, tx).await,
        "bash" | "shell" | "sh" => run_process("bash", &["-c", code], timeout_secs, cwd, tx).await,
        "powershell" | "ps" => {
            if cfg!(windows) {
                run_process(
                    "powershell",
                    &["-NoProfile", "-NonInteractive", "-Command", code],
                    timeout_secs,
                    cwd,
                    tx,
                )
                .await
            } else {
                run_process("bash", &["-c", code], timeout_secs, cwd, tx).await
            }
        }
        "lua" => run_lua_embedded(code, timeout_secs, cwd, tx).await,
        "javascript" | "js" | "node" => {
            run_script(code, "node", ".js", timeout_secs, cwd, tx).await
        }
        other => {
            let msg = format!("不支持的类型: {}\n", other);
            let _ = tx.send(msg.clone());
            Ok((msg, 1))
        }
    }
}

/// Run a script by writing to a temp file with the given extension, then executing it.
async fn run_script(
    code: &str,
    interpreter: &str,
    ext: &str,
    timeout_secs: u64,
    cwd: Option<&str>,
    tx: &UnboundedSender<String>,
) -> Result<(String, i32)> {
    use std::env;
    use tokio::fs;

    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(format!("pc_agent_{}{}", uuid_short(), ext));
    fs::write(&tmp_path, code).await?;

    let tmp_str = tmp_path.to_str().unwrap_or("script");
    let result = run_process(interpreter, &[tmp_str], timeout_secs, cwd, tx).await;

    let _ = fs::remove_file(&tmp_path).await;
    result
}

/// Run Lua code using the built-in mlua (Lua 5.4) interpreter — no external `lua` binary needed.
///
/// Security model:
/// - Runs on a dedicated blocking thread (`spawn_blocking`), isolated from the async runtime.
/// - Lua VM is created fresh per invocation (no shared state between executions).
/// - Only `ALL_SAFE` stdlib is loaded: math, string, table, utf8, coroutine, base.
///   Dangerous libs are excluded: `io`, `os`, `package`/`require`, `debug`, `ffi`.
/// - Timeout is enforced by abandoning the blocking task result (the thread finishes
///   naturally but its output is discarded after the deadline).
async fn run_lua_embedded(
    code: &str,
    timeout_secs: u64,
    cwd: Option<&str>,
    tx: &UnboundedSender<String>,
) -> Result<(String, i32)> {
    use mlua::prelude::*;
    use mlua::Variadic;

    let code_owned = code.to_string();
    let cwd_owned = cwd.map(|s| s.to_string());
    let tx_clone = tx.clone();

    let blocking = tokio::task::spawn_blocking(move || -> (String, i32) {
        let lua = match Lua::new_with(LuaStdLib::ALL_SAFE, LuaOptions::default()) {
            Ok(l) => l,
            Err(e) => return (format!("[lua error] {}\n", e), 1),
        };

        // Capture print() output
        let mut full_output = String::new();
        let tx_print = tx_clone.clone();
        // We use a channel to collect output from the closure without shared mut ref
        let (out_tx, out_rx) = std::sync::mpsc::channel::<String>();

        let out_tx_print = out_tx.clone();
        let print_fn = lua.create_function(move |_, args: Variadic<LuaValue>| {
            let parts: Vec<String> = args.iter().map(lua_value_to_string).collect();
            let line = parts.join("\t") + "\n";
            let _ = out_tx_print.send(line.clone());
            let _ = tx_print.send(line);
            Ok(())
        });

        let out_tx_write = out_tx.clone();
        let tx_write = tx_clone.clone();

        match print_fn {
            Ok(f) => {
                let _ = lua.globals().set("print", f);
            }
            Err(e) => return (format!("[lua error] {}\n", e), 1),
        }

        // Override io.write as well
        if let Ok(io) = lua.globals().get::<LuaTable>("io") {
            let write_fn = lua.create_function(move |_, args: Variadic<LuaValue>| {
                let s: String = args.iter().map(lua_value_to_string).collect();
                let _ = out_tx_write.send(s.clone());
                let _ = tx_write.send(s);
                Ok(())
            });
            if let Ok(f) = write_fn {
                let _ = io.set("write", f);
            }
        }

        // Inject cwd as a global if provided
        if let Some(ref dir) = cwd_owned {
            let _ = lua.globals().set("CWD", dir.as_str());
        }

        // Execute
        let result = lua.load(&code_owned).exec();
        drop(out_tx); // close sender so we can drain

        for chunk in out_rx.try_iter() {
            full_output.push_str(&chunk);
        }

        match result {
            Ok(_) => (full_output, 0),
            Err(e) => {
                let err_msg = format!("[lua error] {}\n", e);
                full_output.push_str(&err_msg);
                (full_output, 1)
            }
        }
    });

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);

    tokio::select! {
        result = blocking => {
            let (output, code) = result.unwrap_or_else(|e| (format!("[panic] {}\n", e), 1));
            let icon = if code == 0 { "✅" } else { "❌" };
            let _ = tx.send(format!("[Status] {} Exit Code: {}\n", icon, code));
            Ok((output, code))
        }
        _ = tokio::time::sleep_until(deadline) => {
            let msg = format!("\n[Timeout Error] 超时强制终止 ({}s)\n", timeout_secs);
            let _ = tx.send(msg.clone());
            Ok((msg, 124))
        }
    }
}

fn lua_value_to_string(v: &mlua::Value) -> String {
    match v {
        mlua::Value::String(s) => s.to_str().map(|b| b.to_string()).unwrap_or_default(),
        mlua::Value::Integer(n) => n.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Nil => "nil".to_string(),
        other => format!("{:?}", other),
    }
}

/// Run Python code by writing to a temp file, then executing it.
/// Mirrors Python's behavior: `python -X utf8 -u temp_file.ai.py`
async fn run_python(
    code: &str,
    timeout_secs: u64,
    cwd: Option<&str>,
    tx: &UnboundedSender<String>,
) -> Result<(String, i32)> {
    use std::env;
    use tokio::fs;

    let tmp_dir = env::temp_dir();
    let tmp_path = tmp_dir.join(format!("pc_agent_{}.ai.py", uuid_short()));
    fs::write(&tmp_path, code).await?;

    let python_cmd = if which_available("python3") {
        "python3"
    } else {
        "python"
    };
    let tmp_str = tmp_path.to_str().unwrap_or("script.ai.py");

    let result = run_process(
        python_cmd,
        &["-X", "utf8", "-u", tmp_str],
        timeout_secs,
        cwd,
        tx,
    )
    .await;

    let _ = fs::remove_file(&tmp_path).await;
    result
}

/// Core process runner with streaming output and timeout.
async fn run_process(
    cmd: &str,
    args: &[&str],
    timeout_secs: u64,
    cwd: Option<&str>,
    tx: &UnboundedSender<String>,
) -> Result<(String, i32)> {
    let mut command = Command::new(cmd);
    command.args(args);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    if let Some(dir) = cwd {
        let abs = std::fs::canonicalize(dir).unwrap_or_else(|_| std::path::PathBuf::from(dir));
        command.current_dir(&abs);
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn()?;

    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut full_output = String::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done {
            break;
        }
        tokio::select! {
            line = stdout_reader.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(l)) => {
                        let formatted = format!("{}\n", l);
                        full_output.push_str(&formatted);
                        let _ = tx.send(formatted);
                    }
                    Ok(None) => { stdout_done = true; }
                    Err(e) => {
                        let _ = tx.send(format!("[stdout error: {}]\n", e));
                        stdout_done = true;
                    }
                }
            }
            line = stderr_reader.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(l)) => {
                        let formatted = format!("[stderr] {}\n", l);
                        full_output.push_str(&formatted);
                        let _ = tx.send(formatted);
                    }
                    Ok(None) => { stderr_done = true; }
                    Err(e) => {
                        let _ = tx.send(format!("[stderr error: {}]\n", e));
                        stderr_done = true;
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let msg = format!("\n[Timeout Error] 超时强制终止 ({}s)\n", timeout_secs);
                full_output.push_str(&msg);
                let _ = tx.send(msg);
                let _ = child.kill().await;
                return Ok((full_output, 124));
            }
        }
    }

    let exit_status = child.wait().await?;
    let exit_code = exit_status.code().unwrap_or(-1);

    let status_icon = if exit_code == 0 { "✅" } else { "❌" };
    let _ = tx.send(format!(
        "[Status] {} Exit Code: {}\n",
        status_icon, exit_code
    ));

    Ok((full_output, exit_code))
}

fn which_available(cmd: &str) -> bool {
    std::process::Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", nanos)
}
