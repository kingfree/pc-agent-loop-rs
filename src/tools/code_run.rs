use anyhow::Result;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

/// Execute code (Python or bash) as a subprocess, streaming output.
/// Mirrors Python's `do_code_run` tool.
///
/// Args:
/// - `language`: "python" | "bash" (default: "python")
/// - `code`: code string to execute
/// - `timeout`: timeout in seconds (default: 30)
pub async fn code_run(
    args: &Value,
    tx: &UnboundedSender<String>,
) -> Result<(String, i32)> {
    let language = args.get("language")
        .and_then(|l| l.as_str())
        .unwrap_or("python");

    let code = args.get("code")
        .or_else(|| args.get("script"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let timeout_secs = args.get("timeout")
        .and_then(|t| t.as_u64())
        .unwrap_or(30);

    debug!("code_run: language={}, code_len={}", language, code.len());

    if code.is_empty() {
        return Ok(("No code provided".to_string(), 1));
    }

    // Determine the command to use
    let (cmd, cmd_args): (&str, Vec<&str>) = match language {
        "bash" | "shell" | "sh" => ("bash", vec!["-c", code]),
        "python" | "python3" | "py" => ("python3", vec!["-c", code]),
        _ => {
            let msg = format!("Unsupported language: {}", language);
            let _ = tx.send(msg.clone());
            return Ok((msg, 1));
        }
    };

    let mut child = Command::new(cmd)
        .args(&cmd_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut output = String::new();

    // Stream output with timeout
    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        tokio::select! {
            line = stdout_reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let formatted = format!("{}\n", l);
                        output.push_str(&formatted);
                        let _ = tx.send(formatted);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(format!("[stdout error: {}]\n", e));
                        break;
                    }
                }
            }
            line = stderr_reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let formatted = format!("[stderr] {}\n", l);
                        output.push_str(&formatted);
                        let _ = tx.send(formatted);
                    }
                    Ok(None) => {} // stderr EOF, continue
                    Err(e) => {
                        let _ = tx.send(format!("[stderr error: {}]\n", e));
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                let msg = format!("[TIMEOUT after {}s]\n", timeout_secs);
                output.push_str(&msg);
                let _ = tx.send(msg);
                let _ = child.kill().await;
                return Ok((output, 124)); // 124 = timeout exit code
            }
        }
    }

    // Drain any remaining stderr
    while let Ok(Some(line)) = stderr_reader.next_line().await {
        let formatted = format!("[stderr] {}\n", line);
        output.push_str(&formatted);
        let _ = tx.send(formatted);
    }

    let exit_status = child.wait().await?;
    let exit_code = exit_status.code().unwrap_or(-1);

    if exit_code != 0 {
        let msg = format!("[Process exited with code {}]\n", exit_code);
        output.push_str(&msg);
        let _ = tx.send(msg);
    }

    Ok((output, exit_code))
}
