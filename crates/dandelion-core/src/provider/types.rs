use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolSource {
    Builtin,
    Mcp { server_id: String },
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub source: ToolSource,
}

impl ToolSpec {
    pub fn builtin(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            source: ToolSource::Builtin,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompletionRequest {
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub reasoning_effort: Option<String>,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<serde_json::Value>,
}

/// An opaque, already validated visual input.  This belongs to the user
/// message, rather than to a provider request side-channel, so every agent
/// loop iteration observes the same structured turn input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputImage {
    pub data_url: String,
}

#[derive(Clone, Debug)]
pub enum ChatMessage {
    User {
        content: String,
        images: Vec<InputImage>,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub struct AssistantMessage {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
}

/// Provider-declared behavior that the loop may rely on. Missing capabilities
/// must degrade to `false`; callers may never infer them from a model name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub parallel_tool_calls: bool,
    pub strict_tool_schema: bool,
    pub input_cache: bool,
    pub resumable_stream: bool,
    pub images: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderDialect {
    #[default]
    OpenAiChatCompletions,
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Cancelled,
    Other(String),
}

/// Provider-neutral stream contract consumed by the single agent loop.
#[derive(Clone, Debug)]
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    /// Only metadata, never partial tool arguments. Lets hosts show progress
    /// while a large file body is still being generated.
    ToolCallProgress {
        name: String,
        argument_bytes: usize,
    },
    /// Chat Completions analogue of Codex `OutputItemDone`: this `tool_call`
    /// will not receive more argument deltas (next index started, or stream ended).
    ToolCallReady(ToolCall),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Finish(FinishReason),
}

/// Compatibility name retained while H1a migrates adapters incrementally.
pub type StreamEvent = ModelEvent;
