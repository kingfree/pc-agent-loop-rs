use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent_loop::StepOutcome;
use crate::llm::MockResponse;

/// Trait mirroring Python's BaseHandler.
/// All tool dispatch is done through this trait.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Called before a tool method executes.
    async fn tool_before_callback(
        &mut self,
        tool_name: &str,
        args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<()> {
        let _ = (tool_name, args, response, tx);
        Ok(())
    }

    /// Called after a tool method returns.
    /// `outcome` is mutable so callbacks can patch `next_prompt` (e.g., PROTOCOL_VIOLATION).
    async fn tool_after_callback(
        &mut self,
        tool_name: &str,
        args: &Value,
        response: &MockResponse,
        outcome: &mut StepOutcome,
        tx: &UnboundedSender<String>,
    ) -> Result<()> {
        let _ = (tool_name, args, response, outcome, tx);
        Ok(())
    }

    /// Optionally patch the next prompt before it's sent.
    fn next_prompt_patcher(&self, next_prompt: &str, outcome: &StepOutcome, turn: usize) -> String {
        let _ = (outcome, turn);
        next_prompt.to_string()
    }

    /// Set the current turn number.
    fn set_current_turn(&mut self, turn: usize);

    /// Dispatch a tool call by name.
    async fn dispatch(
        &mut self,
        tool_name: &str,
        args: &Value,
        response: &MockResponse,
        tx: &UnboundedSender<String>,
    ) -> Result<StepOutcome>;
}

pub mod generic;
pub use generic::GenericAgentHandler;
