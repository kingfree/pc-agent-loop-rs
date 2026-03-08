use anyhow::{anyhow, Result};
use serde_json::Value;
use std::path::Path;
use tokio::fs;
use tracing::debug;

/// Read a file with optional line numbers, keyword search, and pagination.
/// Mirrors Python's `do_file_read` tool.
///
/// Args:
/// - `path`: file path
/// - `start_line`: 1-based start line (default: 1)
/// - `end_line`: 1-based end line (default: all)
/// - `keyword`: if provided, search for keyword and show context
/// - `show_line_numbers`: show line numbers (default: true)
pub async fn file_read(args: &Value) -> Result<String> {
    let path = args.get("path")
        .or_else(|| args.get("file"))
        .or_else(|| args.get("filename"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_read: 'path' argument is required"))?;

    let show_line_numbers = args.get("show_line_numbers")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let keyword = args.get("keyword")
        .and_then(|k| k.as_str());

    debug!("file_read: path={}, keyword={:?}", path, keyword);

    if !Path::new(path).exists() {
        return Err(anyhow!("File not found: {}", path));
    }

    let content = fs::read_to_string(path).await?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    if let Some(kw) = keyword {
        // Keyword search with context
        let kw_lower = kw.to_lowercase();
        let context = args.get("context_lines")
            .and_then(|c| c.as_u64())
            .unwrap_or(5) as usize;

        let mut matching_ranges: Vec<(usize, usize)> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line.to_lowercase().contains(&kw_lower) {
                let start = i.saturating_sub(context);
                let end = (i + context + 1).min(total_lines);
                // Merge with last range if overlapping
                if let Some(last) = matching_ranges.last_mut() {
                    if start <= last.1 {
                        last.1 = end;
                        continue;
                    }
                }
                matching_ranges.push((start, end));
            }
        }

        if matching_ranges.is_empty() {
            return Ok(format!("Keyword '{}' not found in {} ({} lines)", kw, path, total_lines));
        }

        let mut result = format!("File: {} ({} lines) - matches for '{}':\n", path, total_lines, kw);
        for (start, end) in matching_ranges {
            result.push_str(&format!("--- lines {}-{} ---\n", start + 1, end));
            for i in start..end {
                if show_line_numbers {
                    result.push_str(&format!("{:4}: {}\n", i + 1, lines[i]));
                } else {
                    result.push_str(&format!("{}\n", lines[i]));
                }
            }
        }
        return Ok(result);
    }

    // Pagination
    let start_line = args.get("start_line")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).saturating_sub(1))
        .unwrap_or(0);

    let end_line = args.get("end_line")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(total_lines))
        .unwrap_or(total_lines);

    let max_lines = args.get("max_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(500) as usize;

    let actual_end = end_line.min(start_line + max_lines);

    let mut result = format!("File: {} ({} lines)", path, total_lines);
    if start_line > 0 || actual_end < total_lines {
        result.push_str(&format!(", showing lines {}-{}", start_line + 1, actual_end));
    }
    result.push('\n');

    for i in start_line..actual_end {
        if show_line_numbers {
            result.push_str(&format!("{:4}: {}\n", i + 1, lines[i]));
        } else {
            result.push_str(&format!("{}\n", lines[i]));
        }
    }

    if actual_end < total_lines {
        result.push_str(&format!("\n... {} more lines (use start_line={} to continue)\n",
            total_lines - actual_end, actual_end + 1));
    }

    Ok(result)
}

/// Find unique old_content in file and replace with new_content.
/// Mirrors Python's `do_file_patch` tool.
///
/// Args:
/// - `path`: file path
/// - `old_content`: text to find (must be unique in file)
/// - `new_content`: replacement text
pub async fn file_patch(args: &Value) -> Result<String> {
    let path = args.get("path")
        .or_else(|| args.get("file"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'path' is required"))?;

    let old_content = args.get("old_content")
        .or_else(|| args.get("old"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'old_content' is required"))?;

    let new_content = args.get("new_content")
        .or_else(|| args.get("new"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("file_patch: 'new_content' is required"))?;

    debug!("file_patch: path={}", path);

    if !Path::new(path).exists() {
        return Err(anyhow!("File not found: {}", path));
    }

    let content = fs::read_to_string(path).await?;

    // Count occurrences to ensure uniqueness
    let count = content.matches(old_content).count();
    if count == 0 {
        return Err(anyhow!(
            "file_patch: old_content not found in {}.\nFirst 100 chars of old_content: {}",
            path,
            &old_content[..old_content.len().min(100)]
        ));
    }
    if count > 1 {
        return Err(anyhow!(
            "file_patch: old_content found {} times in {} (must be unique). Make the search text more specific.",
            count, path
        ));
    }

    let new_file_content = content.replacen(old_content, new_content, 1);
    fs::write(path, &new_file_content).await?;

    let old_lines = old_content.lines().count();
    let new_lines = new_content.lines().count();
    Ok(format!(
        "Patched {} successfully: replaced {} lines with {} lines.",
        path, old_lines, new_lines
    ))
}

/// Overwrite, append, or prepend a file.
/// Content can be extracted from `<file_content>` tags or code blocks.
/// Mirrors Python's `do_file_write` tool.
///
/// Args:
/// - `path`: file path
/// - `content`: file content (may contain <file_content> tags or ``` blocks)
/// - `mode`: "overwrite" | "append" | "prepend" (default: "overwrite")
pub async fn file_write(args: &Value) -> Result<String> {
    let path = args.get("path")
        .or_else(|| args.get("file"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("file_write: 'path' is required"))?;

    let raw_content = args.get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("file_write: 'content' is required"))?;

    let mode = args.get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("overwrite");

    // Extract actual content from <file_content> tags if present
    let content = extract_file_content(raw_content);

    debug!("file_write: path={}, mode={}, content_len={}", path, mode, content.len());

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
            Ok(format!("Appended {} bytes to {}", content.len(), path))
        }
        "prepend" => {
            let existing = if Path::new(path).exists() {
                fs::read_to_string(path).await?
            } else {
                String::new()
            };
            let new_content = format!("{}{}", content, existing);
            fs::write(path, &new_content).await?;
            Ok(format!("Prepended {} bytes to {}", content.len(), path))
        }
        _ => {
            // overwrite
            fs::write(path, &content).await?;
            Ok(format!("Wrote {} bytes to {}", content.len(), path))
        }
    }
}

/// Extract file content from <file_content> tags or code blocks.
fn extract_file_content(raw: &str) -> String {
    // Try <file_content>...</file_content>
    if let Some(start) = raw.find("<file_content>") {
        if let Some(end) = raw.find("</file_content>") {
            let inner = &raw[start + "<file_content>".len()..end];
            return inner.trim_start_matches('\n').to_string();
        }
        // Unclosed tag - take everything after it
        let inner = &raw[start + "<file_content>".len()..];
        return inner.trim_start_matches('\n').to_string();
    }

    // Try ```lang\n...\n``` or ```\n...\n```
    if raw.starts_with("```") {
        let without_fence = raw.trim_start_matches('`');
        // Skip language identifier line if present
        if let Some(newline_pos) = without_fence.find('\n') {
            let after_lang = &without_fence[newline_pos + 1..];
            // Remove trailing ```
            let trimmed = after_lang.trim_end();
            if trimmed.ends_with("```") {
                return trimmed[..trimmed.len() - 3].to_string();
            }
            return after_lang.to_string();
        }
    }

    raw.to_string()
}
