//! OpenAI Responses SSE event normalization.
//!
//! This module is deliberately transport-free in H1a: it converts official
//! Responses event payloads into the provider-neutral event contract. HTTP
//! request construction is added only after this converter is covered by
//! replay fixtures.

use super::types::{AssistantMessage, FinishReason, ModelEvent, ToolCall};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default)]
pub struct ResponsesEventDecoder {
    text: String,
    calls: Vec<ToolCall>,
    completed_call_ids: HashSet<String>,
}

impl ResponsesEventDecoder {
    pub fn apply(&mut self, event: &Value) -> Option<ModelEvent> {
        let kind = event.get("type")?.as_str()?;
        match kind {
            "response.output_text.delta" => {
                let delta = event.get("delta")?.as_str()?.to_string();
                self.text.push_str(&delta);
                Some(ModelEvent::TextDelta(delta))
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => event
                .get("delta")
                .and_then(Value::as_str)
                .map(|delta| ModelEvent::ReasoningDelta(delta.to_string())),
            "response.function_call_arguments.done" => {
                let id = event.get("call_id")?.as_str()?.to_string();
                let name = event.get("name")?.as_str()?.to_string();
                let arguments = event.get("arguments")?.as_str()?.to_string();
                // Completion on the wire is not enough by itself: only a
                // syntactically complete JSON argument payload may cross the
                // adapter boundary into early tool dispatch.
                if !matches!(
                    serde_json::from_str::<Value>(&arguments),
                    Ok(Value::Object(_))
                ) || !self.completed_call_ids.insert(id.clone())
                {
                    return None;
                }
                let call = ToolCall {
                    id,
                    name,
                    arguments,
                };
                self.calls.push(call.clone());
                Some(ModelEvent::ToolCallReady(call))
            }
            "response.completed" => Some(ModelEvent::Finish(FinishReason::Stop)),
            _ => None,
        }
    }

    pub fn finish(self) -> AssistantMessage {
        let finish_reason = if self.calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };
        AssistantMessage {
            text: self.text,
            tool_calls: self.calls,
            finish_reason: Some(finish_reason.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_done_event_makes_a_tool_call_executable() {
        let mut decoder = ResponsesEventDecoder::default();
        assert!(decoder
            .apply(&json!({"type":"response.function_call_arguments.delta","delta":"{"}))
            .is_none());
        let event = decoder.apply(&json!({
            "type":"response.function_call_arguments.done", "call_id":"call_1",
            "name":"write_file", "arguments":"{\"path\":\"a.txt\"}"
        }));
        assert!(matches!(event, Some(ModelEvent::ToolCallReady(ref call)) if call.id == "call_1"));
        assert_eq!(decoder.finish().tool_calls.len(), 1);
    }

    #[test]
    fn preserves_text_and_marks_completed_function_call_turn() {
        let mut decoder = ResponsesEventDecoder::default();
        assert!(matches!(
            decoder.apply(&json!({"type":"response.output_text.delta", "delta":"done"})),
            Some(ModelEvent::TextDelta(delta)) if delta == "done"
        ));
        decoder.apply(&json!({
            "type":"response.function_call_arguments.done", "call_id":"call_1",
            "name":"read_file", "arguments":"{\"path\":\"README.md\"}"
        }));
        let message = decoder.finish();
        assert_eq!(message.text, "done");
        assert_eq!(message.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn invalid_or_duplicate_done_event_cannot_dispatch_twice() {
        let mut decoder = ResponsesEventDecoder::default();
        let invalid = json!({
            "type":"response.function_call_arguments.done", "call_id":"call_1",
            "name":"write_file", "arguments":"{"
        });
        assert!(decoder.apply(&invalid).is_none());
        let done = json!({
            "type":"response.function_call_arguments.done", "call_id":"call_1",
            "name":"write_file", "arguments":"{\"path\":\"a.txt\"}"
        });
        assert!(matches!(
            decoder.apply(&done),
            Some(ModelEvent::ToolCallReady(_))
        ));
        assert!(decoder.apply(&done).is_none());
        assert_eq!(decoder.finish().tool_calls.len(), 1);
    }

    #[test]
    fn preserves_reasoning_then_emits_only_the_finalized_call() {
        let mut decoder = ResponsesEventDecoder::default();
        assert!(matches!(
            decoder.apply(&json!({"type":"response.reasoning_text.delta", "delta":"inspect"})),
            Some(ModelEvent::ReasoningDelta(delta)) if delta == "inspect"
        ));
        assert!(decoder
            .apply(&json!({
                "type":"response.function_call_arguments.delta", "delta":"{"
            }))
            .is_none());
        assert!(matches!(
            decoder.apply(&json!({
                "type":"response.function_call_arguments.done", "call_id":"call_2",
                "name":"read_file", "arguments":"{\"path\":\"src/lib.rs\"}"
            })),
            Some(ModelEvent::ToolCallReady(call)) if call.id == "call_2"
        ));
        assert!(matches!(
            decoder.apply(&json!({"type":"response.completed"})),
            Some(ModelEvent::Finish(FinishReason::Stop))
        ));
    }
}
