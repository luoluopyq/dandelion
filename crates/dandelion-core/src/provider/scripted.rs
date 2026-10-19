use super::types::{
    AssistantMessage, ChatMessage, CompletionRequest, ProviderCapabilities, ProviderDialect,
    StreamEvent, ToolCall,
};
use super::Provider;
use crate::{Error, Result};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct ScriptedTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub wait_cancel: bool,
    pub fail: Option<String>,
    pub delay: Duration,
}

impl ScriptedTurn {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            finish_reason: Some("stop".into()),
            wait_cancel: false,
            fail: None,
            delay: Duration::ZERO,
        }
    }

    pub fn tools(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            text: text.into(),
            tool_calls,
            finish_reason: Some("tool_calls".into()),
            wait_cancel: false,
            fail: None,
            delay: Duration::ZERO,
        }
    }

    pub fn truncated(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_calls: Vec::new(),
            finish_reason: Some("length".into()),
            wait_cancel: false,
            fail: None,
            delay: Duration::ZERO,
        }
    }

    pub fn hang_until_cancel() -> Self {
        Self {
            text: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            wait_cancel: true,
            fail: None,
            delay: Duration::ZERO,
        }
    }

    pub fn overflow() -> Self {
        Self {
            text: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            wait_cancel: false,
            fail: Some("context overflow: prompt too long".into()),
            delay: Duration::ZERO,
        }
    }

    pub fn delayed_text(text: impl Into<String>, delay: Duration) -> Self {
        let mut t = Self::text(text);
        t.delay = delay;
        t
    }
}

#[derive(Clone, Debug)]
pub struct ScriptedComplete {
    pub tool_names: Vec<String>,
    pub system: String,
    pub temperature: f32,
    pub model: String,
}

/// Deterministic Provider for tests. Each `complete` pops one turn.
pub struct ScriptedProvider {
    turns: Mutex<VecDeque<ScriptedTurn>>,
    last_system: Mutex<Option<String>>,
    last_tool_names: Mutex<Vec<String>>,
    complete_count: AtomicUsize,
    history: Mutex<Vec<ScriptedComplete>>,
    last_messages: Mutex<Vec<ChatMessage>>,
}

impl ScriptedProvider {
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            last_system: Mutex::new(None),
            last_tool_names: Mutex::new(Vec::new()),
            complete_count: AtomicUsize::new(0),
            history: Mutex::new(Vec::new()),
            last_messages: Mutex::new(Vec::new()),
        }
    }

    pub fn last_system(&self) -> Option<String> {
        self.last_system.lock().expect("lock").clone()
    }

    pub fn last_tool_names(&self) -> Vec<String> {
        self.last_tool_names.lock().expect("lock").clone()
    }

    pub fn complete_count(&self) -> usize {
        self.complete_count.load(Ordering::SeqCst)
    }

    pub fn history(&self) -> Vec<ScriptedComplete> {
        self.history.lock().expect("lock").clone()
    }

    pub fn last_messages(&self) -> Vec<ChatMessage> {
        self.last_messages.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAiChatCompletions
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            parallel_tool_calls: true,
            strict_tool_schema: false,
            input_cache: false,
            resumable_stream: false,
            images: true,
        }
    }

    async fn complete(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<AssistantMessage> {
        {
            let names: Vec<String> = req
                .tools
                .iter()
                .filter_map(|t| {
                    t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            *self.last_system.lock().expect("lock") = Some(req.system.clone());
            *self.last_tool_names.lock().expect("lock") = names.clone();
            *self.last_messages.lock().expect("lock") = req.messages.clone();
            self.history.lock().expect("lock").push(ScriptedComplete {
                tool_names: names,
                system: req.system.clone(),
                temperature: req.temperature,
                model: req.model.clone(),
            });
            self.complete_count.fetch_add(1, Ordering::SeqCst);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let turn = self
            .turns
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| ScriptedTurn::text(""));
        if turn.delay > Duration::ZERO {
            tokio::select! {
                _ = tokio::time::sleep(turn.delay) => {}
                _ = cancel.cancelled() => return Err(Error::Cancelled),
            }
        }
        if let Some(msg) = turn.fail {
            return Err(Error::Provider(msg));
        }
        if turn.wait_cancel {
            cancel.cancelled().await;
            return Err(Error::Cancelled);
        }
        if !turn.text.is_empty() {
            let _ = events.send(StreamEvent::TextDelta(turn.text.clone())).await;
        }
        for call in &turn.tool_calls {
            let _ = events.send(StreamEvent::ToolCallReady(call.clone())).await;
        }
        Ok(AssistantMessage {
            text: turn.text,
            tool_calls: turn.tool_calls,
            finish_reason: turn.finish_reason,
        })
    }
}
