use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;
use tokio::fs;
use tracing::debug;

/// Read a file with optional line numbers, keyword search, and pagination.
/// Mirrors Python's `file_read` function in ga.py.
///
/// Args (matching Python original):
/// - `path`: file path
/// - `start`: 1-based start line (default: 1)
/// - `count`: number of lines to read (default: 200)
/// - `keyword`: if provided, return first match (case-insensitive) and context
/// - `show_linenos`: show line numbers in `line|content` format (default: true)
pub async fn file_read(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .or_else(|| args.get("filename"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_read: 'path' argument is required"))?;

    // Support both Python names and legacy Rust names
    let show_linenos = args
        .get("show_linenos")
        .or_else(|| args.get("show_line_numbers"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // `start` (Python) or `start_line` (legacy)
    let start = args
        .get("start")
        .or_else(|| args.get("start_line"))
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1)
        .max(1);

    // `count` (Python) or `max_lines` (legacy)
    let count = args
        .get("count")
        .or_else(|| args.get("max_lines"))
        .and_then(|v| v.as_u64())
        .unwrap_or(200) as usize;

    let keyword = args.get("keyword").and_then(|k| k.as_str());

    debug!(
        "file_read: path={}, start={}, count={}, keyword={:?}",
        path, start, count, keyword
    );

    if !Path::new(path).exists() {
        return Ok(format!("Error: 文件不存在: {}", path));
    }

    let content = fs::read_to_string(path).await?;
    let all_lines: Vec<&str> = content.lines().collect();
    let total_lines = all_lines.len();

    if let Some(kw) = keyword {
        // Keyword search: find first match (case-insensitive), return surrounding context.
        // Mirrors Python's deque-based lookahead buffer.
        let kw_lower = kw.to_lowercase();
        let start_idx = start.saturating_sub(1);
        let context_before = count / 3;

        let mut before_buf: std::collections::VecDeque<(usize, &&str)> =
            std::collections::VecDeque::with_capacity(context_before + 1);

        for (i, line) in all_lines.iter().enumerate().skip(start_idx) {
            if line.to_lowercase().contains(&kw_lower) {
                let before: Vec<(usize, &&str)> = before_buf.iter().cloned().collect();
                let remaining_count = count.saturating_sub(before.len() + 1);
                let after: Vec<(usize, &&str)> = all_lines
                    .iter()
                    .enumerate()
                    .skip(i + 1)
                    .take(remaining_count)
                    .collect();

                let mut result = String::new();
                for (j, l) in before
                    .iter()
                    .chain(std::iter::once(&(i, line)))
                    .chain(after.iter())
                {
                    let lineno = j + 1;
                    if show_linenos {
                        result.push_str(&format!("{}|{}\n", lineno, truncate_line(l)));
                    } else {
                        result.push_str(&format!("{}\n", l));
                    }
                }
                return Ok(result);
            }
            if before_buf.len() >= context_before {
                before_buf.pop_front();
            }
            before_buf.push_back((i, line));
        }

        // Fallback: return from `start`
        let fallback_msg = format!(
            "Keyword '{}' not found after line {}. Falling back to content from line {}:\n\n",
            kw, start, start
        );
        let fallback = file_read_range(&all_lines, total_lines, start, count, show_linenos);
        return Ok(fallback_msg + &fallback);
    }

    // Normal paginated read
    Ok(file_read_range(
        &all_lines,
        total_lines,
        start,
        count,
        show_linenos,
    ))
}

/// Format lines for output, matching Python's `line_number|content` format.
fn file_read_range(
    all_lines: &[&str],
    total_lines: usize,
    start: usize,
    count: usize,
    show_linenos: bool,
) -> String {
    let start_idx = start.saturating_sub(1);
    let end_idx = (start_idx + count).min(total_lines);
    let shown = &all_lines[start_idx..end_idx];
    let real_count = shown.len();
    let remaining = total_lines.saturating_sub(end_idx);

    // Max chars per line to avoid huge outputs (mirrors Python L_MAX = max(100, 512000//realcnt))
    let l_max = if real_count == 0 {
        512
    } else {
        (512000 / real_count).max(100)
    };

    let mut result = if show_linenos {
        // Python header: "[FILE] Total X lines\n"
        if remaining >= 5000 {
            format!("[FILE] Total {}+ lines\n", total_lines)
        } else {
            format!("[FILE] Total {} lines\n", total_lines)
        }
    } else {
        String::new()
    };

    let mut has_truncated = false;
    for (i, line) in shown.iter().enumerate() {
        let lineno = start_idx + i + 1;
        if show_linenos {
            let (truncated, was_truncated) = truncate_line_max(line, l_max);
            if was_truncated {
                has_truncated = true;
            }
            result.push_str(&format!("{}|{}\n", lineno, truncated));
        } else {
            result.push_str(&format!("{}\n", line));
        }
    }

    if has_truncated {
        result.push_str("\n\n（某些行被截断，如需完整内容可改用 code_run 读取）");
    }

    result
}

fn truncate_line(line: &str) -> &str {
    if line.len() <= 512 {
        line
    } else {
        &line[..512]
    }
}

fn truncate_line_max(line: &str, max: usize) -> (String, bool) {
    if line.len() <= max {
        (line.to_string(), false)
    } else {
        (format!("{} ... [TRUNCATED]", &line[..max]), true)
    }
}

/// Find unique old_content in file and replace with new_content.
/// Mirrors Python's `file_patch` function in ga.py.
///
/// Args:
/// - `path`: file path
/// - `old_content`: text to find (must be unique in file)
/// - `new_content`: replacement text
pub async fn file_patch(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'path' is required"))?;

    let old_content = args
        .get("old_content")
        .or_else(|| args.get("old"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'old_content' is required"))?;

    let new_content = args
        .get("new_content")
        .or_else(|| args.get("new"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'new_content' is required"))?;

    debug!("file_patch: path={}", path);

    if !Path::new(path).exists() {
        return Ok(r#"{"status": "error", "msg": "文件不存在"}"#.to_string());
    }

    if old_content.is_empty() {
        return Ok(
            r#"{"status": "error", "msg": "old_content 为空，请确认 arguments"}"#.to_string(),
        );
    }

    let content = fs::read_to_string(path).await?;
    let count = content.matches(old_content).count();

    if count == 0 {
        return Ok(r#"{"status": "error", "msg": "未找到匹配的旧文本块，建议：先用 file_read 确认当前内容，再分小段进行 patch。若多次失败则询问用户，严禁自行使用 overwrite 或代码替换。"}"#.to_string());
    }
    if count > 1 {
        return Ok(format!(
            r#"{{"status": "error", "msg": "找到 {} 处匹配，无法确定唯一位置。请提供更长、更具体的旧文本块以确保唯一性。建议：包含上下文行来增强特征，或分小段逐个修改。"}}"#,
            count
        ));
    }

    let new_file_content = content.replacen(old_content, new_content, 1);
    fs::write(path, &new_file_content).await?;

    Ok(r#"{"status": "success", "msg": "文件局部修改成功"}"#.to_string())
}

/// Overwrite, append, or prepend a file.
/// Content extracted from response `<file_content>` tags or code blocks at handler level.
/// Mirrors Python's `do_file_write` tool.
///
/// Args:
/// - `path`: file path
/// - `content`: file content (may contain <file_content> tags or ``` blocks)
/// - `mode`: "overwrite" | "append" | "prepend" (default: "overwrite")
pub async fn file_write(args: &Value) -> Result<String> {
    let path = args
        .get("path")
        .or_else(|| args.get("file"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_write: 'path' is required"))?;

    let raw_content = args
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("file_write: 'content' is required"))?;

    let mode = args
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("overwrite");

    // Extract actual content from <file_content> tags if present
    let content = extract_file_content(raw_content);

    debug!(
        "file_write: path={}, mode={}, content_len={}",
        path,
        mode,
        content.len()
    );

    // Ensure parent directory exists
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }

    match mode {
        "append" => {
            let mut existing = if Path::new(path).exists() {
                fs::read_to_string(path).await?
            } else {
                String::new()
            };
            existing.push_str(&content);
            fs::write(path, &existing).await?;
            Ok(format!(
                r#"{{"status": "success", "writed_bytes": {}}}"#,
                content.len()
            ))
        }
        "prepend" => {
            let existing = if Path::new(path).exists() {
                fs::read_to_string(path).await?
            } else {
                String::new()
            };
            let new_content = format!("{}{}", content, existing);
            fs::write(path, &new_content).await?;
            Ok(format!(
                r#"{{"status": "success", "writed_bytes": {}}}"#,
                content.len()
            ))
        }
        _ => {
            // overwrite
            fs::write(path, &content).await?;
            Ok(format!(
                r#"{{"status": "success", "writed_bytes": {}}}"#,
                content.len()
            ))
        }
    }
}

/// Extract file content from <file_content> tags or code blocks.
/// Mirrors Python's `extract_robust_content` in ga.py.
pub fn extract_file_content(raw: &str) -> String {
    // Try <file_content>...</file_content>
    if let (Some(s), Some(e)) = (raw.find("<file_content>"), raw.rfind("</file_content>")) {
        if s < e {
            let inner = &raw[s + "<file_content>".len()..e];
            return inner.trim_start_matches('\n').to_string();
        }
    }
    // Unclosed tag: take everything after it
    if let Some(s) = raw.find("<file_content>") {
        let inner = &raw[s + "<file_content>".len()..];
        return inner.trim_start_matches('\n').to_string();
    }

    // Try ```...``` code blocks
    let s = raw.find("```");
    let e = raw.rfind("```");
    if let (Some(s), Some(e)) = (s, e) {
        if s < e {
            // Skip language identifier line
            let after_fence = &raw[s + 3..];
            let content_start = after_fence.find('\n').map(|p| p + 1).unwrap_or(0);
            let inner = &after_fence[content_start..];
            let end_pos = inner.rfind("```").unwrap_or(inner.len());
            return inner[..end_pos].trim_end_matches('\n').to_string();
        }
    }

    raw.to_string()
}
