pub mod factory;
pub mod openai_sse;
pub mod responses;
pub mod responses_sse;
pub mod scripted;
pub mod types;

use crate::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use types::{AssistantMessage, CompletionRequest, StreamEvent};

#[async_trait]
pub trait Provider: Send + Sync {
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAiChatCompletions
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    async fn complete(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
        events: mpsc::Sender<StreamEvent>,
    ) -> Result<AssistantMessage>;
}

pub use factory::ProviderFactory;
pub use openai_sse::{OpenAiSse, ProviderProbe};
pub use responses_sse::OpenAiResponsesSse;
pub use scripted::{ScriptedComplete, ScriptedProvider, ScriptedTurn};
pub use types::{FinishReason, ModelEvent, ProviderCapabilities, ProviderDialect};
