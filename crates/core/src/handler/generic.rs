use anyhow::Result;
use async_trait::async_trait;
use chrono::Local;
use regex::Regex;
use reqwest;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::agent_loop::StepOutcome;
use crate::handler::Handler;
use crate::llm::MockResponse;
use crate::tools::{code_run, file_patch, file_read, file_write};
use crate::tools::file_ops::extract_file_content;

/// GenericAgentHandler: Implements all tool handlers.
/// Mirrors Python's GenericAgentHandler / ga.py
pub struct GenericAgentHandler {
    pub current_turn: usize,
    pub history_info: Vec<String>,
    pub key_info: String,
    pub related_sop: String,
    pub task_description: String,
    pub work_dir: String,
    pub ask_user_callback: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
}

impl GenericAgentHandler {
    pub fn new(task_description: &str, work_dir: &str) -> Self {
        GenericAgentHandler {
            current_turn: 0,
            history_info: Vec::new(),
            key_info: String::new(),
            related_sop: String::new(),
            task_description: task_description.to_string(),
            work_dir: work_dir.to_string(),
            ask_user_callback: None,
        }
    }

    /// Resolve a relative path against work_dir.
    fn get_abs_path(&self, path: &str) -> String {
        if path.is_empty() { return String::new(); }
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            path.to_string()
        } else {
            std::path::Path::new(&self.work_dir)
                .join(path)
                .to_string_lossy()
                .to_string()
        }
    }

    /// Read global memory insight file and structure.
    /// Mirrors Python's `get_global_memory()`.
    pub fn get_global_memory(&self) -> String {
        let base = std::path::Path::new(&self.work_dir);
        let insight_path = base.join("memory").join("global_mem_insight.txt");
        let structure_path = base.join("assets").join("insight_fixed_structure.txt");

        let mut prompt = String::from("\n");
        if let Ok(insight_text) = std::fs::read_to_string(&insight_path) {
            let structure = std::fs::read_to_string(&structure_path).unwrap_or_default();
            prompt.push_str("\n[Memory]\n");
            prompt.push_str(&format!("cwd = {} （用./引用）\n", base.join("temp").display()));
            if !structure.is_empty() {
                prompt.push_str(&structure);
                prompt.push('\n');
            }
            prompt.push_str("../memory/global_mem_insight.txt:\n");
            prompt.push_str(&insight_text);
            prompt.push('\n');
        }
        prompt
    }

    /// Returns anchor prompt with history, key_info, and turn info.
    /// Matches Python's `_get_anchor_prompt` exactly.
    pub fn get_anchor_prompt(&self) -> String {
        let history_slice: Vec<&str> = self.history_info.iter()
            .rev().take(20).collect::<Vec<_>>()
            .into_iter().rev()
            .map(|s| s.as_str())
            .collect();
        let h_str = history_slice.join("\n");

        let mut prompt = format!("\n### [WORKING MEMORY]\n<history>\n{}\n</history>", h_str);
        prompt.push_str(&format!("\nCurrent turn: {}\n", self.current_turn));
        if !self.key_info.is_empty() {
            prompt.push_str(&format!("\n<key_info>{}</key_info>", self.key_info));
        }
        if !self.related_sop.is_empty() {
            prompt.push_str(&format!("\n有不清晰的地方请再次读取{}", self.related_sop));
        }
        prompt
    }

    /// Extract code from response content code blocks (matching Python behavior).
    /// Python extracts from ```{type}\n...\n``` blocks in the response content.
    /// Returns the last matching block (Python uses matches[-1]).
    fn extract_code_from_response(response_content: &str, code_type: &str) -> Option<String> {
        // Match ```{code_type}\n...\n```
        let escaped = regex::escape(code_type);
        let pattern = format!(r"(?s)```{}\n(.*?)\n```", escaped);
        if let Ok(re) = Regex::new(&pattern) {
            let matches: Vec<String> = re.captures_iter(response_content)
                .map(|c| c[1].trim().to_string())
                .collect();
            if !matches.is_empty() {
                return matches.into_iter().last();
            }
        }
        // Also try plain ``` blocks
        if let Ok(re_plain) = Regex::new(r"(?s)```\n(.*?)\n```") {
            let matches: Vec<String> = re_plain.captures_iter(response_content)
                .map(|c| c[1].trim().to_string())
                .collect();
            if !matches.is_empty() {
                return matches.into_iter().last();
            }
        }
        None
    }

    async fn do_code_run(
        &mut self,
        args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let code_type = args.get("type")
            .or_else(|| args.get("language"))
            .and_then(|l| l.as_str())
            .unwrap_or("python");

        // Extract code from response content first (Python behavior)
        let (code, warning) = if let Some(extracted) =
            Self::extract_code_from_response(&response.content, code_type)
        {
            (extracted, String::new())
        } else if let Some(code_arg) = args.get("code").or_else(|| args.get("script")).and_then(|c| c.as_str()) {
            (code_arg.to_string(), "\n下次要记得先在回复正文中提供代码块，而不是放在参数中".to_string())
        } else {
            return Ok(StepOutcome::next(None, format!(
                "【系统错误】：你调用了 code_run，但未在先在回复正文中提供 ```{} 代码块。请重新输出代码并附带工具调用。",
                code_type
            )));
        };

        // Resolve cwd relative to work_dir
        let raw_cwd = args.get("cwd").and_then(|c| c.as_str()).unwrap_or("./");
        let cwd = self.get_abs_path(raw_cwd);

        let mut effective_args = serde_json::json!({
            "type": code_type,
            "code": code,
            "cwd": cwd,
        });
        if let Some(timeout) = args.get("timeout") {
            effective_args["timeout"] = timeout.clone();
        }

        let (output, exit_code) = code_run(&effective_args, tx).await?;
        let next_prompt = format!(
            "代码执行完毕，退出码: {}\n{}{}",
            exit_code, warning, self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(Some(Value::String(output)), next_prompt))
    }

    async fn do_file_read(
        &mut self,
        args: &Value,
        _tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let abs_path = self.get_abs_path(path);
        let mut effective_args = args.clone();
        if let Some(obj) = effective_args.as_object_mut() {
            obj.insert("path".to_string(), Value::String(abs_path.clone()));
        }

        self.log_memory_access(&abs_path);

        match file_read(&effective_args).await {
            Ok(content) => {
                let show_linenos = args.get("show_linenos")
                    .or_else(|| args.get("show_line_numbers"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let result_str = if show_linenos {
                    format!("由于设置了show_linenos，以下返回信息为：(行号|)内容 。\n{}", content)
                } else {
                    content
                };

                let mut next_prompt = self.get_anchor_prompt();
                if abs_path.contains("memory") || abs_path.contains("sop") {
                    next_prompt.push_str("\n[SYSTEM TIPS] 正在读取记忆或SOP文件，若决定按sop执行请提取sop中的关键点（特别是靠后的）update working memory.");
                }
                Ok(StepOutcome::next(Some(Value::String(result_str)), next_prompt))
            }
            Err(e) => Ok(StepOutcome::next(None,
                format!("文件读取失败: {}\n{}", e, self.get_anchor_prompt())
            )),
        }
    }

    async fn do_file_patch(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let abs_path = self.get_abs_path(path);
        let _ = tx.send(format!("[Action] Patching file: {}\n", abs_path));

        let mut effective_args = args.clone();
        if let Some(obj) = effective_args.as_object_mut() {
            obj.insert("path".to_string(), Value::String(abs_path));
        }

        match file_patch(&effective_args).await {
            Ok(msg) => {
                let _ = tx.send(format!("\n{}\n", msg));
                Ok(StepOutcome::next(Some(Value::String(msg)), self.get_anchor_prompt()))
            }
            Err(e) => Ok(StepOutcome::next(None,
                format!("文件修补失败: {}\n{}", e, self.get_anchor_prompt())
            )),
        }
    }

    async fn do_file_write(
        &mut self,
        args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        let abs_path = self.get_abs_path(path);
        let mode = args.get("mode").and_then(|m| m.as_str()).unwrap_or("overwrite");

        let action_str = match mode {
            "prepend" => "Prepending to",
            "append" => "Appending to",
            _ => "Overwriting",
        };
        let basename = std::path::Path::new(&abs_path).file_name()
            .and_then(|n| n.to_str()).unwrap_or(&abs_path);
        let _ = tx.send(format!("[Action] {} file: {}\n", action_str, basename));

        // Extract content from response (Python behavior: look in response first, then args)
        let content = if let Some(extracted) = extract_content_from_response(&response.content) {
            extracted
        } else if let Some(c) = args.get("content").and_then(|c| c.as_str()) {
            extract_file_content(c).to_string()
        } else {
            let _ = tx.send("[Status] ❌ 失败: 未在回复中找到代码块内容\n".to_string());
            return Ok(StepOutcome::next(
                Some(serde_json::json!({"status": "error", "msg": "No content found, if you want a blank, you should use code_run"})),
                "\n".to_string()
            ));
        };

        let effective_args = serde_json::json!({
            "path": abs_path,
            "content": content,
            "mode": mode
        });

        match file_write(&effective_args).await {
            Ok(_) => {
                let _ = tx.send(format!("[Status] ✅ {} 成功 ({} bytes)\n", mode, content.len()));
                Ok(StepOutcome::next(
                    Some(serde_json::json!({"status": "success", "writed_bytes": content.len()})),
                    self.get_anchor_prompt()
                ))
            }
            Err(e) => {
                let _ = tx.send(format!("[Status] ❌ 写入异常: {}\n", e));
                Ok(StepOutcome::next(
                    Some(serde_json::json!({"status": "error", "msg": e.to_string()})),
                    "\n".to_string()
                ))
            }
        }
    }

    async fn call_webdriver_link(cmd: &str, extra: Value) -> Result<Value> {
        let client = reqwest::Client::new();
        let mut payload = serde_json::json!({"cmd": cmd});
        if let (Some(obj), Some(extra_obj)) = (payload.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
        let resp = client
            .post("http://localhost:18766/link")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        let json: Value = resp.json().await?;
        Ok(json.get("r").cloned().unwrap_or(json))
    }

    async fn do_web_scan(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let tabs_only = args.get("tabs_only").and_then(|v| v.as_bool()).unwrap_or(false);
        let switch_tab_id = args.get("switch_tab_id").and_then(|v| v.as_str()).unwrap_or("");
        let _ = tx.send(format!("[web_scan] tabs_only={}\n", tabs_only));

        let extra = serde_json::json!({
            "tabs_only": tabs_only,
            "switch_tab_id": switch_tab_id
        });
        match Self::call_webdriver_link("web_scan", extra).await {
            Ok(result) => {
                let result_str = serde_json::to_string(&result).unwrap_or_default();
                // Print result without content field (like Python does)
                let mut display = result.clone();
                if let Some(obj) = display.as_object_mut() { obj.remove("content"); }
                let _ = tx.send(format!("[Info] {}\n", serde_json::to_string(&display).unwrap_or_default()));

                let next_prompt = if let Some(content) = result.get("content").and_then(|c| c.as_str()) {
                    format!("<tool_result>\n```html\n{}\n```\n</tool_result>", content)
                } else {
                    "标签页列表如上\n".to_string()
                };
                let _ = result_str;
                Ok(StepOutcome::next(Some(result), next_prompt))
            }
            Err(e) => Ok(StepOutcome::next(None, format!(
                "web_scan 失败 (TMWebDriver 未运行或不可达 http://localhost:18766): {}\n{}",
                e, self.get_anchor_prompt()
            ))),
        }
    }

    async fn do_web_execute_js(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let js = args.get("script").or_else(|| args.get("js"))
            .and_then(|j| j.as_str()).unwrap_or("");

        if js.is_empty() {
            return Ok(StepOutcome::next(None,
                "[Error] Empty script param. Check your tool call arguments.".to_string()
            ));
        }

        // Load from file if js is a file path (matches Python)
        let abs_js = self.get_abs_path(js);
        let script = if std::path::Path::new(&abs_js).is_file() {
            std::fs::read_to_string(&abs_js).unwrap_or_else(|_| js.to_string())
        } else {
            js.to_string()
        };

        let save_to_file = args.get("save_to_file").and_then(|v| v.as_str()).unwrap_or("");
        let switch_tab_id = args.get("switch_tab_id").or_else(|| args.get("tab_id"))
            .and_then(|v| v.as_str()).unwrap_or("");
        let no_monitor = args.get("no_monitor").and_then(|v| v.as_bool()).unwrap_or(false);

        let _ = tx.send(format!("[web_execute_js] JS: {}...\n", &script[..script.len().min(50)]));

        let extra = serde_json::json!({
            "code": script,
            "sessionId": switch_tab_id,
            "no_monitor": no_monitor
        });
        match Self::call_webdriver_link("execute_js", extra).await {
            Ok(mut result) => {
                if !save_to_file.is_empty() {
                    if let Some(js_return) = result.get("js_return").cloned() {
                        let content = match &js_return {
                            Value::String(s) => s.clone(),
                            _ => serde_json::to_string(&js_return).unwrap_or_default(),
                        };
                        let save_path = self.get_abs_path(save_to_file);
                        let saved_msg = match std::fs::write(&save_path, &content) {
                            Ok(_) => format!("{}\n\n[已保存完整内容到 {}]",
                                &content[..content.len().min(170)], save_path),
                            Err(_) => format!("{}\n\n[保存失败，无法写入文件 {}]",
                                content, save_path),
                        };
                        if let Some(obj) = result.as_object_mut() {
                            obj.insert("js_return".to_string(), Value::String(saved_msg));
                        }
                    }
                }
                let result_str = serde_json::to_string_pretty(&result).unwrap_or_default();
                let _ = tx.send(format!("JS 执行结果:\n{}\n",
                    &result_str[..result_str.len().min(500)]));
                Ok(StepOutcome::next(Some(result), self.get_anchor_prompt()))
            }
            Err(e) => Ok(StepOutcome::next(None, format!(
                "web_execute_js 失败 (TMWebDriver 未运行或不可达 http://localhost:18766): {}\n{}",
                e, self.get_anchor_prompt()
            ))),
        }
    }

    async fn do_ask_user(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let question = args.get("question").or_else(|| args.get("msg"))
            .and_then(|q| q.as_str()).unwrap_or("请输入:");
        let candidates: Vec<String> = args.get("candidates")
            .and_then(|c| c.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let _ = tx.send(format!("\n**[ask_user]** {}\n", question));
        if !candidates.is_empty() {
            let _ = tx.send(format!("候选项: {}\n", candidates.join(", ")));
        }
        let _ = tx.send("Waiting for your answer ...\n".to_string());

        let answer = if let Some(ref cb) = self.ask_user_callback {
            cb(question)
        } else {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        };

        Ok(StepOutcome::exit(Some(serde_json::json!({
            "status": "INTERRUPT",
            "intent": "HUMAN_INTERVENTION",
            "data": { "question": question, "answer": answer, "candidates": candidates }
        }))))
    }

    async fn do_update_working_checkpoint(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        if args.get("key_info").is_some() {
            self.key_info = args["key_info"].as_str().unwrap_or("").to_string();
        }
        if args.get("related_sop").is_some() {
            self.related_sop = args["related_sop"].as_str().unwrap_or("").to_string();
        }
        let _ = tx.send("[Info] Updated key_info and related_sop.\n".to_string());
        let _ = tx.send(format!("key_info:\n{}\n\n", self.key_info));
        let _ = tx.send(format!("related_sop:\n{}\n\n", self.related_sop));
        debug!("Checkpoint updated: key_info={} chars", self.key_info.len());
        Ok(StepOutcome::next(
            Some(serde_json::json!({"status": "success"})),
            self.get_anchor_prompt()
        ))
    }

    async fn do_start_long_term_update(
        &mut self,
        _args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let _ = tx.send("[Info] Start distilling good memory for long-term storage.\n".to_string());

        let prompt = format!(
            "### [总结提炼经验] 既然你觉得当前任务有重要信息需要记忆，请提取最近一次任务中【事实验证成功且长期有效】的环境事实、用户偏好、重要步骤，更新记忆。\n\
            本工具是标记开启结算过程，若已在更新记忆过程或没有值得记忆的点，忽略本次调用。\n\
            **提取行动验证成功的信息**：\n\
            - **环境事实**（路径/凭证/配置）→ `file_patch` 更新 L2，同步 L1\n\
            - **复杂任务经验**（关键坑点/前置条件/重要步骤）→ L3 精简 SOP（只记你被坑得多次重试的核心要点）\n\
            **禁止**：临时变量、具体推理过程、未验证信息、通用常识、你可以轻松复现的细节。\n\
            **操作**：严格遵循提供的L0的记忆更新SOP。先 `file_read` 看现有 → 判断类型 → 最小化更新 → 无新内容跳过，保证对记忆库最小局部修改。\n\
            {}",
            self.get_global_memory()
        );

        let sop_path = std::path::Path::new(&self.work_dir)
            .join("memory").join("memory_management_sop.md");
        let result = if sop_path.exists() {
            std::fs::read_to_string(&sop_path)
                .map(Value::String)
                .unwrap_or(Value::String("Error reading SOP".to_string()))
        } else {
            Value::String("Memory Management SOP not found. Do not update memory.".to_string())
        };

        Ok(StepOutcome::next(Some(result), prompt))
    }

    async fn do_no_tool(
        &mut self,
        _args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let content = &response.content;

        // 1. Empty response protection
        if content.trim().is_empty() {
            let _ = tx.send("[Warn] LLM returned an empty response. Retrying...\n".to_string());
            return Ok(StepOutcome::next(
                Some(serde_json::json!({})),
                "[System] 回复为空，请重新生成内容或调用工具。".to_string()
            ));
        }

        // 2. Detect large code block without tool call (matches Python exactly)
        let code_block_re = Regex::new(r"(?s)```[a-zA-Z0-9_]*\n[\s\S]{100,}?```").unwrap();
        if let Some(m) = code_block_re.find(content) {
            let mut residual = content.clone();
            residual = residual.replacen(m.as_str(), "", 1);
            let thinking_re = Regex::new(r"(?si)<thinking>[\s\S]*?</thinking>").unwrap();
            let summary_re = Regex::new(r"(?si)<summary>[\s\S]*?</summary>").unwrap();
            residual = thinking_re.replace_all(&residual, "").to_string();
            residual = summary_re.replace_all(&residual, "").to_string();
            let clean: String = residual.chars().filter(|c| !c.is_whitespace()).collect();
            if clean.len() <= 50 {
                let _ = tx.send("[Info] Detected large code block without tool call. Requesting clarification.\n".to_string());
                return Ok(StepOutcome::next(
                    Some(serde_json::json!({})),
                    "[System] 检测到你在上一轮回复中主要内容是较大代码块（仅配有<thinking>/<summary>），且本轮未调用任何工具。\n\
                    如果这些代码需要执行、写入文件或进一步分析，请重新组织回复并显式调用相应工具\
                    （例如：code_run、file_write、file_patch 等）；\n\
                    如果只是向用户展示或讲解代码片段，请在回复中补充自然语言说明，\
                    并明确是否还需要额外的实际操作。".to_string()
                ));
            }
        }

        // 3. Normal: final response
        let _ = tx.send("[Info] Final response to user.\n".to_string());
        Ok(StepOutcome::exit(Some(Value::String(content.clone()))))
    }

    /// Log memory file accesses (mirrors Python's log_memory_access).
    fn log_memory_access(&self, path: &str) {
        if !path.contains("memory") { return; }
        let stats_file = std::path::Path::new(&self.work_dir)
            .join("memory").join("file_access_stats.json");
        let mut stats: serde_json::Map<String, Value> = if stats_file.exists() {
            std::fs::read_to_string(&stats_file)
                .ok().and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };
        let fname = std::path::Path::new(path).file_name()
            .and_then(|n| n.to_str()).unwrap_or(path).to_string();
        let old_count = stats.get(&fname)
            .and_then(|v| v.get("count")).and_then(|c| c.as_u64()).unwrap_or(0);
        stats.insert(fname, serde_json::json!({
            "count": old_count + 1,
            "last": Local::now().format("%Y-%m-%d").to_string()
        }));
        let _ = std::fs::write(&stats_file,
            serde_json::to_string_pretty(&Value::Object(stats)).unwrap_or_default());
    }
}

/// Extract file content from response text (for file_write).
/// Mirrors Python's `extract_robust_content`.
fn extract_content_from_response(text: &str) -> Option<String> {
    if let (Some(s), Some(e)) = (text.find("<file_content>"), text.rfind("</file_content>")) {
        if s < e {
            return Some(text[s + "<file_content>".len()..e].trim_start_matches('\n').to_string());
        }
    }
    let s_pos = text.find("```");
    let e_pos = text.rfind("```");
    if let (Some(s), Some(e)) = (s_pos, e_pos) {
        if s < e {
            let after = &text[s + 3..];
            let start = after.find('\n').map(|p| p + 1).unwrap_or(0);
            let inner = &after[start..];
            let end = inner.rfind("```").unwrap_or(inner.len());
            return Some(inner[..end].trim_end_matches('\n').to_string());
        }
    }
    None
}

#[async_trait]
impl Handler for GenericAgentHandler {
    fn set_current_turn(&mut self, turn: usize) {
        self.current_turn = turn;
    }

    fn next_prompt_patcher(&self, next_prompt: &str, _outcome: &StepOutcome, turn: usize) -> String {
        let mut result = next_prompt.to_string();
        if turn > 0 && turn % 30 == 0 {
            result.push_str(&format!(
                "\n\n[DANGER] 已连续执行第 {} 轮。你必须总结情况进行ask_user，不允许继续重试。", turn
            ));
        } else if turn > 0 && turn % 7 == 0 {
            result.push_str(&format!(
                "\n\n[DANGER] 已连续执行第 {} 轮。禁止无效重试。若无有效进展，必须切换策略：1. 探测物理边界 2. 请求用户协助。如有需要，可调用 update_working_checkpoint 保存关键上下文。",
                turn
            ));
        } else if turn > 0 && turn % 10 == 0 {
            result.push_str(&self.get_global_memory());
        }
        result
    }

    async fn tool_after_callback(
        &mut self,
        tool_name: &str,
        _args: &Value,
        response: &MockResponse,
        outcome: &mut StepOutcome,
        _tx: &UnboundedSender<String>,
    ) -> Result<()> {
        let summary_re = Regex::new(r"(?s)<summary>(.*?)</summary>").unwrap();
        if let Some(cap) = summary_re.captures(&response.content) {
            let summary = &cap[1];
            let truncated = &summary[..summary.len().min(200)];
            self.history_info.push(format!("[Agent] {}", truncated.trim()));
        } else {
            // Auto-generate summary and add PROTOCOL_VIOLATION (matches Python exactly)
            let auto_summary = if tool_name == "no_tool" {
                "直接回答了用户问题".to_string()
            } else {
                format!("调用工具{}", tool_name)
            };
            self.history_info.push(format!("[Agent] {}", auto_summary));
            if let Some(ref mut np) = outcome.next_prompt {
                np.push_str("\nPROTOCOL_VIOLATION: 上一轮遗漏了<summary>。 我已根据物理动作自动补全。请务必在下次回复中记得<summary>协议。");
            }
        }
        if self.history_info.len() > 30 {
            self.history_info.remove(0);
        }
        Ok(())
    }

    async fn dispatch(
        &mut self,
        tool_name: &str,
        args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        self.tool_before_callback(tool_name, args, response, tx).await?;

        let mut outcome = match tool_name {
            "code_run" | "run_code" | "execute_code" => {
                self.do_code_run(args, response, tx).await?
            }
            "file_read" | "read_file" => self.do_file_read(args, tx).await?,
            "file_patch" | "patch_file" => self.do_file_patch(args, tx).await?,
            "file_write" | "write_file" => self.do_file_write(args, response, tx).await?,
            "web_scan" => self.do_web_scan(args, tx).await?,
            "web_execute_js" => self.do_web_execute_js(args, tx).await?,
            "ask_user" => self.do_ask_user(args, tx).await?,
            "update_working_checkpoint" | "update_checkpoint" => {
                self.do_update_working_checkpoint(args, tx).await?
            }
            "start_long_term_update" | "long_term_update" => {
                self.do_start_long_term_update(args, tx).await?
            }
            "no_tool" => self.do_no_tool(args, response, tx).await?,
            "bad_json" => {
                let msg = args.get("msg").and_then(|m| m.as_str()).unwrap_or("bad_json").to_string();
                let _ = tx.send(format!("[bad_json] {}\n", msg));
                StepOutcome::next(None, msg)
            }
            other => {
                let msg = format!("未知工具: {}", other);
                let _ = tx.send(format!("{}\n", msg));
                StepOutcome::next(None, format!("未知工具 {}", other))
            }
        };

        self.tool_after_callback(tool_name, args, response, &mut outcome, tx).await?;
        Ok(outcome)
    }
}
