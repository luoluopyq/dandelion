use super::{OpenAiResponsesSse, OpenAiSse, Provider, ProviderDialect};
use crate::config::Settings;
use crate::{Error, Result};
use std::sync::Arc;

/// The only construction seam for wire-protocol providers. Runtime receives
/// the trait object and never branches on a provider dialect.
pub struct ProviderFactory;

impl ProviderFactory {
    /// Select a wire protocol from persisted configuration. The backend name
    /// is a transport choice, never a model-name heuristic; legacy compatible
    /// backend labels keep the Chat Completions contract.
    pub fn from_settings(settings: &Settings) -> Result<Arc<dyn Provider>> {
        let dialect = match settings
            .default_backend
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "openai_responses" | "responses" => ProviderDialect::OpenAiResponses,
            _ => ProviderDialect::OpenAiChatCompletions,
        };
        Self::for_dialect(settings, dialect)
    }

    pub fn for_dialect(settings: &Settings, dialect: ProviderDialect) -> Result<Arc<dyn Provider>> {
        match dialect {
            ProviderDialect::OpenAiChatCompletions => {
                Ok(Arc::new(OpenAiSse::from_settings(settings)?))
            }
            ProviderDialect::OpenAiResponses => {
                Ok(Arc::new(OpenAiResponsesSse::from_settings(settings)?))
            }
            ProviderDialect::AnthropicMessages => Err(Error::Invalid(
                "Anthropic Messages transport is not configured yet".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_selects_chat_without_model_name_inference() {
        let mut settings = Settings::defaults();
        settings.base_url = Some("http://127.0.0.1:9999/v1".into());
        let provider =
            ProviderFactory::for_dialect(&settings, ProviderDialect::OpenAiChatCompletions)
                .unwrap();
        assert_eq!(provider.dialect(), ProviderDialect::OpenAiChatCompletions);
        let provider =
            ProviderFactory::for_dialect(&settings, ProviderDialect::OpenAiResponses).unwrap();
        assert_eq!(provider.dialect(), ProviderDialect::OpenAiResponses);
    }

    #[test]
    fn factory_selects_responses_from_explicit_backend() {
        let mut settings = Settings::defaults();
        settings.base_url = Some("http://127.0.0.1:9999/v1".into());
        settings.default_backend = "openai_responses".into();
        assert_eq!(
            ProviderFactory::from_settings(&settings).unwrap().dialect(),
            ProviderDialect::OpenAiResponses
        );
    }
}
