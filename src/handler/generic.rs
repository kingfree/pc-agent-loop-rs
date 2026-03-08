use anyhow::Result;
use async_trait::async_trait;
use chrono::Local;
use regex::Regex;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tracing::debug;

use crate::agent_loop::StepOutcome;
use crate::handler::Handler;
use crate::llm::MockResponse;
use crate::tools::{code_run, file_patch, file_read, file_write};

/// GenericAgentHandler: Implements all tool handlers.
/// Mirrors Python's GenericAgentHandler / ga.py
pub struct GenericAgentHandler {
    pub current_turn: usize,
    pub history_info: Vec<String>,
    pub key_info: String,
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
            task_description: task_description.to_string(),
            work_dir: work_dir.to_string(),
            ask_user_callback: None,
        }
    }

    /// Returns anchor prompt with history, key_info, and turn info.
    pub fn get_anchor_prompt(&self) -> String {
        let mut prompt = String::new();
        if !self.key_info.is_empty() {
            prompt.push_str(&format!("\n### 工作记忆 (Working Memory)\n{}\n", self.key_info));
        }
        if !self.history_info.is_empty() {
            prompt.push_str("\n### 历史摘要 (History)\n");
            for (i, h) in self.history_info.iter().enumerate() {
                prompt.push_str(&format!("{}. {}\n", i + 1, h));
            }
        }
        prompt.push_str(&format!("\n当前第 {} 轮\n", self.current_turn));
        prompt
    }

    async fn do_code_run(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let (output, exit_code) = code_run(args, tx).await?;
        let result = Value::String(output.clone());
        let next_prompt = format!(
            "代码执行完毕，退出码: {}\n{}",
            exit_code,
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(Some(result), next_prompt))
    }

    async fn do_file_read(
        &mut self,
        args: &Value,
        _tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        match file_read(args).await {
            Ok(content) => {
                let next_prompt = format!("文件读取完成。\n{}", self.get_anchor_prompt());
                Ok(StepOutcome::next(Some(Value::String(content)), next_prompt))
            }
            Err(e) => {
                let next_prompt = format!("文件读取失败: {}\n{}", e, self.get_anchor_prompt());
                Ok(StepOutcome::next(None, next_prompt))
            }
        }
    }

    async fn do_file_patch(
        &mut self,
        args: &Value,
        _tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        match file_patch(args).await {
            Ok(msg) => {
                let next_prompt = format!("{}\n{}", msg, self.get_anchor_prompt());
                Ok(StepOutcome::next(None, next_prompt))
            }
            Err(e) => {
                let next_prompt = format!("文件修补失败: {}\n{}", e, self.get_anchor_prompt());
                Ok(StepOutcome::next(None, next_prompt))
            }
        }
    }

    async fn do_file_write(
        &mut self,
        args: &Value,
        _tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        match file_write(args).await {
            Ok(msg) => {
                let next_prompt = format!("{}\n{}", msg, self.get_anchor_prompt());
                Ok(StepOutcome::next(None, next_prompt))
            }
            Err(e) => {
                let next_prompt = format!("文件写入失败: {}\n{}", e, self.get_anchor_prompt());
                Ok(StepOutcome::next(None, next_prompt))
            }
        }
    }

    async fn do_web_scan(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let url = args.get("url").and_then(|u| u.as_str()).unwrap_or("(no url)");
        let _ = tx.send(format!("[web_scan stub] URL: {}\n", url));
        let next_prompt = format!(
            "web_scan 功能暂未实现。URL: {}\n{}",
            url,
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(None, next_prompt))
    }

    async fn do_web_execute_js(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let js = args.get("js")
            .or_else(|| args.get("script"))
            .and_then(|j| j.as_str())
            .unwrap_or("(no js)");
        let _ = tx.send(format!("[web_execute_js stub] JS: {}...\n", &js[..js.len().min(50)]));
        let next_prompt = format!(
            "web_execute_js 功能暂未实现。\n{}",
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(None, next_prompt))
    }

    async fn do_ask_user(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let question = args.get("question")
            .or_else(|| args.get("msg"))
            .and_then(|q| q.as_str())
            .unwrap_or("请输入:");

        let _ = tx.send(format!("\n**[ask_user]** {}\n", question));

        // Use callback if provided, otherwise read from stdin
        let answer = if let Some(ref cb) = self.ask_user_callback {
            cb(question)
        } else {
            // Read from stdin
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        };

        let next_prompt = format!(
            "用户回答: {}\n{}",
            answer,
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(Some(Value::String(answer)), next_prompt))
    }

    async fn do_update_working_checkpoint(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let new_key_info = args.get("key_info")
            .or_else(|| args.get("content"))
            .or_else(|| args.get("checkpoint"))
            .and_then(|k| k.as_str())
            .unwrap_or("");

        self.key_info = new_key_info.to_string();
        let _ = tx.send(format!("[checkpoint updated: {} bytes]\n", new_key_info.len()));
        debug!("Working checkpoint updated: {} chars", new_key_info.len());

        let next_prompt = format!(
            "工作记忆已更新。\n{}",
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(None, next_prompt))
    }

    async fn do_start_long_term_update(
        &mut self,
        args: &Value,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let content = args.get("content")
            .or_else(|| args.get("summary"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        // In the Rust implementation, we do a simple in-memory consolidation
        let timestamp = Local::now().format("%Y-%m-%d %H:%M").to_string();
        let entry = format!("[{}] {}", timestamp, content);
        self.history_info.push(entry.clone());

        // Keep history bounded
        if self.history_info.len() > 20 {
            self.history_info.remove(0);
        }

        let _ = tx.send(format!("[long_term_update: {}]\n", &entry[..entry.len().min(80)]));

        let next_prompt = format!(
            "长期记忆已更新。\n{}",
            self.get_anchor_prompt()
        );
        Ok(StepOutcome::next(None, next_prompt))
    }

    async fn do_no_tool(
        &mut self,
        _args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome> {
        let content = &response.content;

        // Check if the response contains a large code block without calling a tool
        let has_code_block = content.contains("```") && content.len() > 200;

        if has_code_block {
            let _ = tx.send("[no_tool: large code block detected, prompting to use a tool]\n".to_string());
            let next_prompt = format!(
                "你的回复包含了代码，但没有调用任何工具。请使用 code_run 或 file_write 工具来执行或保存代码。\n{}",
                self.get_anchor_prompt()
            );
            Ok(StepOutcome::next(None, next_prompt))
        } else {
            // No tool call and no code block → task is done
            let _ = tx.send("[no_tool: task done]\n".to_string());
            Ok(StepOutcome::done(Some(Value::String(content.clone()))))
        }
    }
}

#[async_trait]
impl Handler for GenericAgentHandler {
    fn set_current_turn(&mut self, turn: usize) {
        self.current_turn = turn;
    }

    fn next_prompt_patcher(
        &self,
        next_prompt: &str,
        _outcome: &StepOutcome,
        turn: usize,
    ) -> String {
        // Add danger warnings at certain turn counts
        let mut result = next_prompt.to_string();
        if turn == 10 {
            result.push_str("\n\n⚠️ **警告**: 已进行10轮，请注意控制步骤数量，尽快完成任务。");
        } else if turn >= 12 {
            result.push_str("\n\n🚨 **紧急**: 轮数即将耗尽，必须立即完成或退出！");
        }
        result
    }

    async fn tool_after_callback(
        &mut self,
        tool_name: &str,
        _args: &Value,
        response: &MockResponse,
        _outcome: &StepOutcome,
        _tx: &UnboundedSender<String>,
    ) -> Result<()> {
        // Extract <summary> from response and log to history
        let summary_re = Regex::new(r"(?s)<summary>(.*?)</summary>").unwrap();
        if let Some(cap) = summary_re.captures(&response.raw_text) {
            let summary = cap[1].trim().to_string();
            if !summary.is_empty() {
                let entry = format!("[Turn {}][{}] {}", self.current_turn, tool_name, summary);
                self.history_info.push(entry);
                // Keep bounded
                if self.history_info.len() > 30 {
                    self.history_info.remove(0);
                }
            }
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
        // Call before callback
        self.tool_before_callback(tool_name, args, response, tx).await?;

        let outcome = match tool_name {
            "code_run" | "run_code" | "execute_code" => {
                self.do_code_run(args, tx).await?
            }
            "file_read" | "read_file" => {
                self.do_file_read(args, tx).await?
            }
            "file_patch" | "patch_file" => {
                self.do_file_patch(args, tx).await?
            }
            "file_write" | "write_file" => {
                self.do_file_write(args, tx).await?
            }
            "web_scan" => {
                self.do_web_scan(args, tx).await?
            }
            "web_execute_js" => {
                self.do_web_execute_js(args, tx).await?
            }
            "ask_user" => {
                self.do_ask_user(args, tx).await?
            }
            "update_working_checkpoint" | "update_checkpoint" => {
                self.do_update_working_checkpoint(args, tx).await?
            }
            "start_long_term_update" | "long_term_update" => {
                self.do_start_long_term_update(args, tx).await?
            }
            "no_tool" => {
                self.do_no_tool(args, response, tx).await?
            }
            "bad_json" => {
                let msg = args.get("msg")
                    .and_then(|m| m.as_str())
                    .unwrap_or("bad_json")
                    .to_string();
                let _ = tx.send(format!("[bad_json] {}\n", msg));
                StepOutcome::next(None, msg)
            }
            other => {
                let msg = format!("未知工具: {}", other);
                let _ = tx.send(format!("{}\n", msg));
                StepOutcome::next(None, format!("未知工具 {}", other))
            }
        };

        // Call after callback
        self.tool_after_callback(tool_name, args, response, &outcome, tx).await?;

        Ok(outcome)
    }
}
