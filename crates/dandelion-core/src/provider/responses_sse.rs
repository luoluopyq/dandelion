//! OpenAI Responses API transport.  The decoder stays separate in
//! `responses.rs`, so wire framing cannot leak into the agent loop.

use super::responses::ResponsesEventDecoder;
use super::types::{
    AssistantMessage, ChatMessage, CompletionRequest, ProviderCapabilities, ProviderDialect,
    StreamEvent,
};
use super::Provider;
use crate::config::Settings;
use crate::{Error, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Streaming adapter for the OpenAI Responses wire protocol.
pub struct OpenAiResponsesSse {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    sse_idle_timeout: Duration,
}

impl OpenAiResponsesSse {
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let base_url = settings
            .base_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::NotConfigured("dandelion.baseUrl".into()))?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(settings.http_connect_timeout_ms))
            .build()
            .map_err(|err| Error::Provider(err.to_string()))?;
        Ok(Self {
            client,
            base_url: Settings::normalize_base_url(base_url),
            api_key: settings.api_key.clone(),
            sse_idle_timeout: Duration::from_millis(settings.sse_idle_timeout_ms),
        })
    }

    fn url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    fn body(req: &CompletionRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "instructions": req.system,
            "stream": true,
            "temperature": req.temperature,
            "max_output_tokens": req.max_tokens,
            "input": req.messages.iter().flat_map(response_input).collect::<Vec<_>>(),
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.iter().map(response_tool).collect());
        }
        if let Some(effort) = &req.reasoning_effort {
            body["reasoning"] = json!({"effort": effort});
        }
        body
    }
}

#[async_trait]
impl Provider for OpenAiResponsesSse {
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAiResponses
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            parallel_tool_calls: true,
            strict_tool_schema: true,
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
        let mut request = self
            .client
            .post(self.url())
            .header("Content-Type", "application/json")
            .json(&Self::body(&req));
        if let Some(key) = self.api_key.as_deref().filter(|key| !key.is_empty()) {
            request = request.bearer_auth(key);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = request.send() => result.map_err(|err| Error::Provider(err.to_string()))?,
        };
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Provider(format!(
                "HTTP {status}: {}",
                response.text().await.unwrap_or_default()
            )));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut decoder = ResponsesEventDecoder::default();
        let mut saw_frame = false;
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                result = timeout(self.sse_idle_timeout, stream.next()) => result,
            };
            match next {
                Ok(Some(Ok(chunk))) => {
                    buffer.extend_from_slice(&chunk);
                    consume_lines(&mut buffer, &mut decoder, &events, &mut saw_frame).await?;
                }
                Ok(Some(Err(err))) => {
                    return Err(Error::Provider(format!("Responses SSE interrupted: {err}")))
                }
                Ok(None) => break,
                Err(_) => return Err(Error::Provider("Responses SSE idle timeout".into())),
            }
        }
        // A valid last SSE field need not be newline-terminated. Normalizing
        // it here avoids silently dropping the final completed tool call.
        if !buffer.is_empty() {
            buffer.push(b'\n');
        }
        consume_lines(&mut buffer, &mut decoder, &events, &mut saw_frame).await?;
        if !saw_frame {
            return Err(Error::Provider(
                "Responses SSE stream had no data frames".into(),
            ));
        }
        Ok(decoder.finish())
    }
}

fn response_tool(tool: &Value) -> Value {
    let function = tool.get("function").unwrap_or(tool);
    json!({
        "type": "function",
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
        // Dandelion's existing builtin/MCP schemas use the portable JSON
        // Schema subset accepted by Chat Completions. Keep that behavior
        // explicit while H1a validates a stricter schema dialect separately.
        "strict": false,
    })
}

fn response_input(message: &ChatMessage) -> Vec<Value> {
    match message {
        ChatMessage::User { content, images } => {
            let mut parts = vec![json!({"type":"input_text", "text": content})];
            parts.extend(
                images
                    .iter()
                    .map(|image| json!({"type":"input_image", "image_url": image.data_url})),
            );
            vec![json!({"role":"user", "content": parts})]
        }
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => {
            let mut items = Vec::new();
            if let Some(text) = content.as_deref().filter(|text| !text.is_empty()) {
                items.push(
                    json!({"role":"assistant", "content":[{"type":"output_text", "text":text}]}),
                );
            }
            items.extend(tool_calls.iter().map(|call| json!({
                "type":"function_call", "call_id":call.id, "name":call.name, "arguments":call.arguments
            })));
            items
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => vec![json!({
            "type":"function_call_output", "call_id":tool_call_id, "output":content
        })],
    }
}

async fn consume_lines(
    buffer: &mut Vec<u8>,
    decoder: &mut ResponsesEventDecoder,
    events: &mpsc::Sender<StreamEvent>,
    saw_frame: &mut bool,
) -> Result<()> {
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=end).collect::<Vec<_>>();
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        let line = String::from_utf8_lossy(&line);
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        *saw_frame = true;
        let frame: Value = serde_json::from_str(data)
            .map_err(|err| Error::Provider(format!("invalid Responses SSE json: {err}")))?;
        if matches!(
            frame.get("type").and_then(Value::as_str),
            Some("error" | "response.failed" | "response.incomplete")
        ) {
            // Do not surface remote error text: it can be provider-generated
            // request echo. Failing the turn is safer than treating a failed
            // response as a normal stop and proceeding with partial output.
            return Err(Error::Provider(
                "Responses API reported an unsuccessful response".into(),
            ));
        }
        if let Some(event) = decoder.apply(&frame) {
            let _ = events.send(event).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{FinishReason, InputImage, ToolCall};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "gpt-test".into(),
            temperature: 0.2,
            max_tokens: 128,
            reasoning_effort: Some("low".into()),
            system: "system".into(),
            messages: vec![
                ChatMessage::User {
                    content: "look".into(),
                    images: vec![InputImage {
                        data_url: "data:image/png;base64,AA".into(),
                    }],
                },
                ChatMessage::Assistant {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "write_file".into(),
                        arguments: "{\"path\":\"a\"}".into(),
                    }],
                },
                ChatMessage::Tool {
                    tool_call_id: "call_1".into(),
                    content: "ok".into(),
                },
            ],
            tools: vec![
                json!({"type":"function", "function":{"name":"write_file", "description":"write", "parameters":{"type":"object"}}}),
            ],
        }
    }

    #[test]
    fn maps_history_tools_and_images_to_responses_input() {
        let body = OpenAiResponsesSse::body(&request());
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["name"], "write_file");
        assert_eq!(body["tools"][0]["strict"], false);
        assert_eq!(body["reasoning"]["effort"], "low");
    }

    #[tokio::test]
    async fn streams_completed_call_from_responses_endpoint_without_trailing_newline() {
        let server = MockServer::start().await;
        let frames = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_1\",\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}\n\n",
            "data: {\"type\":\"response.completed\"}"
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(frames),
            )
            .mount(&server)
            .await;
        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.http_connect_timeout_ms = 5_000;
        settings.sse_idle_timeout_ms = 5_000;
        let provider = OpenAiResponsesSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let message = provider
            .complete(request(), CancellationToken::new(), tx)
            .await
            .expect("complete Responses stream");
        assert_eq!(message.text, "done");
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].name, "read_file");
        assert_eq!(message.finish_reason.as_deref(), Some("tool_calls"));
        assert!(matches!(rx.recv().await, Some(StreamEvent::TextDelta(delta)) if delta == "done"));
        assert!(
            matches!(rx.recv().await, Some(StreamEvent::ToolCallReady(call)) if call.id == "call_1")
        );
        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::Finish(FinishReason::Stop))
        ));
    }

    #[tokio::test]
    async fn failed_response_event_fails_the_turn_without_dispatching() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: {\"type\":\"response.failed\"}\n\n"),
            )
            .mount(&server)
            .await;
        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        let provider = OpenAiResponsesSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(2);
        let err = provider
            .complete(request(), CancellationToken::new(), tx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsuccessful response"));
        assert!(rx.try_recv().is_err());
    }
}
