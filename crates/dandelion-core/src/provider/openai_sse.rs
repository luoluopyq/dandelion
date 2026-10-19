use super::types::{
    AssistantMessage, ChatMessage, CompletionRequest, FinishReason, ProviderCapabilities,
    ProviderDialect, StreamEvent, ToolCall,
};
use super::Provider;
use crate::config::Settings;
use crate::{Error, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProbe {
    pub ok: bool,
    pub status: u16,
    pub message: String,
    pub models: Vec<String>,
}

pub struct OpenAiSse {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    sse_idle_timeout: Duration,
    retry_max: u32,
    retry_base_ms: u64,
    retry_cap_ms: u64,
    dynamic_token_budget: bool,
    token_budget_margin_tokens: u32,
    token_budget_min_output_tokens: u32,
    tokenize_timeout: Duration,
}

#[derive(Deserialize)]
struct TokenizeResponse {
    count: u64,
    max_model_len: u64,
}

fn output_budget(
    requested: u32,
    prompt: u64,
    context: u64,
    margin: u32,
    minimum: u32,
) -> Result<u32> {
    if context == 0 {
        return Err(Error::Provider(
            "vLLM /tokenize returned max_model_len=0".into(),
        ));
    }
    let available = context
        .saturating_sub(prompt)
        .saturating_sub(u64::from(margin));
    if available < u64::from(minimum.min(requested)) {
        return Err(Error::Provider(format!(
            "context overflow: prompt uses {prompt} of {context} tokens; only {available} output tokens remain after the {margin}-token margin"
        )));
    }
    Ok(u64::from(requested).min(available) as u32)
}

fn parse_model_ids(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    if let Some(arr) = value.get("data").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_string))
            .collect();
    }
    if let Some(arr) = value.get("models").and_then(|v| v.as_array()) {
        return arr
            .iter()
            .filter_map(|m| {
                m.get("name")
                    .or_else(|| m.get("id"))
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .collect();
    }
    Vec::new()
}

impl OpenAiSse {
    pub fn from_settings(settings: &Settings) -> Result<Self> {
        let base_url = settings
            .base_url
            .as_ref()
            .filter(|s| !s.is_empty())
            .cloned()
            .ok_or_else(|| Error::NotConfigured("dandelion.baseUrl".into()))?;
        Self::from_base_url(&base_url, settings)
    }

    pub fn from_base_url(base_url: &str, settings: &Settings) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(settings.http_connect_timeout_ms))
            .build()
            .map_err(|err| Error::Provider(err.to_string()))?;
        Ok(Self {
            client,
            base_url: Settings::normalize_base_url(base_url),
            api_key: settings.api_key.clone(),
            sse_idle_timeout: Duration::from_millis(settings.sse_idle_timeout_ms),
            retry_max: settings.retry_max,
            retry_base_ms: settings.retry_base_ms,
            retry_cap_ms: settings.retry_cap_ms,
            dynamic_token_budget: settings.dynamic_token_budget
                && settings.default_backend.eq_ignore_ascii_case("vllm"),
            token_budget_margin_tokens: settings.token_budget_margin_tokens,
            token_budget_min_output_tokens: settings.token_budget_min_output_tokens,
            tokenize_timeout: Duration::from_millis(settings.http_connect_timeout_ms),
        })
    }

    /// GET `{base}/models`. Not an agent loop. Never logs the API key.
    pub async fn probe(
        base_url: &str,
        api_key: Option<&str>,
        connect_timeout_ms: u64,
    ) -> Result<ProviderProbe> {
        let base = Settings::normalize_base_url(base_url);
        if base.is_empty() {
            return Err(Error::NotConfigured("dandelion.baseUrl".into()));
        }
        let timeout_ms = connect_timeout_ms.max(1);
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(timeout_ms))
            .timeout(Duration::from_millis(
                timeout_ms.saturating_mul(2).max(8_000),
            ))
            .build()
            .map_err(|err| Error::Provider(err.to_string()))?;
        let url = format!("{}/models", base.trim_end_matches('/'));
        let mut builder = client.get(&url);
        if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        match builder.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let ok = resp.status().is_success();
                let body = resp.text().await.unwrap_or_default();
                let models = parse_model_ids(&body);
                let message = if ok {
                    if models.is_empty() {
                        "reachable".into()
                    } else {
                        format!("{} models", models.len())
                    }
                } else {
                    body.chars().take(200).collect()
                };
                Ok(ProviderProbe {
                    ok,
                    status,
                    message,
                    models,
                })
            }
            Err(err) => Ok(ProviderProbe {
                ok: false,
                status: 0,
                message: err.without_url().to_string(),
                models: Vec::new(),
            }),
        }
    }

    fn url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn tokenize_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let root = base.strip_suffix("/v1").unwrap_or(base);
        format!("{root}/tokenize")
    }

    async fn apply_token_budget(
        &self,
        body: &mut Value,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if !self.dynamic_token_budget {
            return Ok(());
        }
        let mut tokenize = json!({
            "model": req.model,
            "messages": body["messages"],
            "add_generation_prompt": true,
            "return_token_strs": false,
        });
        if let Some(tools) = body.get("tools") {
            tokenize["tools"] = tools.clone();
        }
        if let Some(kwargs) = body.get("chat_template_kwargs") {
            tokenize["chat_template_kwargs"] = kwargs.clone();
        }
        let mut builder = self.client.post(self.tokenize_url()).json(&tokenize);
        if let Some(key) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        let request = async {
            let response = builder
                .send()
                .await
                .map_err(|err| Error::Provider(format!("vLLM /tokenize: {}", err.without_url())))?;
            if !response.status().is_success() {
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                return Err(Error::Provider(format!(
                    "vLLM /tokenize HTTP {status}: {detail}"
                )));
            }
            response
                .json::<TokenizeResponse>()
                .await
                .map_err(|err| Error::Provider(format!("vLLM /tokenize response: {err}")))
        };
        let counted = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = timeout(self.tokenize_timeout, request) => {
                result.map_err(|_| Error::Provider("vLLM /tokenize timed out".into()))??
            }
        };
        let budget = output_budget(
            req.max_tokens,
            counted.count,
            counted.max_model_len,
            self.token_budget_margin_tokens,
            self.token_budget_min_output_tokens,
        )?;
        tracing::debug!(
            prompt_tokens = counted.count,
            context_tokens = counted.max_model_len,
            output_tokens = budget,
            "vLLM token budget"
        );
        body["max_tokens"] = json!(budget);
        Ok(())
    }

    fn body(req: &CompletionRequest) -> Value {
        let mut messages = Vec::new();
        messages.push(json!({
            "role": "system",
            "content": req.system,
        }));
        for m in &req.messages {
            messages.push(chat_to_json(m));
        }
        let mut body = json!({
            "model": req.model,
            "stream": true,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.clone());
        }
        if req.model == "default_model" {
            // mlx_lm.server names its CLI-loaded model `default_model` and
            // forwards these values to the tokenizer's chat template. The
            // generic Chat Completions reasoning_effort field is ignored there.
            // An omitted effort is the visible-answer recovery request.
            let mut kwargs = json!({"enable_thinking": req.reasoning_effort.is_some()});
            if let Some(effort) = &req.reasoning_effort {
                kwargs["reasoning_effort"] = json!(effort);
            }
            body["chat_template_kwargs"] = kwargs;
        } else if req.model.to_ascii_lowercase().contains("nemotron-3-nano") {
            // Nemotron 3 Nano controls its thinking path through the chat
            // template, not OpenAI's generic reasoning_effort field. On the
            // initial call `reasoning_effort` is present; the reasoning-only
            // recovery call deliberately omits it so the template emits the
            // user-facing answer directly.
            body["chat_template_kwargs"] = json!({
                "enable_thinking": req.reasoning_effort.is_some()
            });
        } else if req.model.to_ascii_lowercase().contains("qwen3") {
            // Qwen3 hybrid-reasoning chat templates (vLLM and similar) toggle
            // the thinking path through `chat_template_kwargs.enable_thinking`
            // only — the generic top-level `reasoning_effort` field does not
            // silence thinking there, and thinking defaults to ON on such
            // deployments. An omitted effort therefore has to send the
            // explicit off-switch, or "thinking off" turns (Wiki builds,
            // recovery requests) spend the whole output budget on invisible
            // reasoning and return no visible text. Verified against the
            // deployment template: enable_thinking=false ⇒ zero reasoning
            // tokens. A configured effort keeps today's top-level behavior.
            if let Some(effort) = &req.reasoning_effort {
                body["reasoning_effort"] = json!(effort);
            } else {
                body["chat_template_kwargs"] = json!({"enable_thinking": false});
            }
        } else if let Some(effort) = &req.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        body
    }
}

#[async_trait]
impl Provider for OpenAiSse {
    fn dialect(&self) -> ProviderDialect {
        ProviderDialect::OpenAiChatCompletions
    }
    fn capabilities(&self) -> ProviderCapabilities {
        // These describe the Chat Completions wire contract we actually send,
        // not any presumed model-specific behavior.
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
        let mut body = Self::body(&req);
        self.apply_token_budget(&mut body, &req, &cancel).await?;
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match self.attempt(&body, &cancel, &events).await {
                Ok(msg) => return Ok(msg),
                Err(_err) if cancel.is_cancelled() => return Err(Error::Cancelled),
                Err(err) if err.to_string().contains("SSE stream had no choices") => {
                    tracing::warn!(error = %err, "empty SSE response; trying a non-streaming completion");
                    return self.attempt_nonstream(&body, &cancel, &events).await;
                }
                Err(err) if retryable(&err) && attempt < self.retry_max => {
                    let delay = retry_delay(self.retry_base_ms, self.retry_cap_ms, attempt);
                    attempt += 1;
                    tracing::warn!(attempt, delay_ms = delay, error = %err, "provider retry");
                    let _ = events
                        .send(StreamEvent::ToolCallProgress {
                            name: String::new(),
                            argument_bytes: 0,
                        })
                        .await;
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(Error::Cancelled),
                        _ = sleep(Duration::from_millis(delay)) => {}
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl OpenAiSse {
    async fn attempt_nonstream(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        events: &mpsc::Sender<StreamEvent>,
    ) -> Result<AssistantMessage> {
        let mut request = body.clone();
        request["stream"] = json!(false);
        let mut builder = self.client.post(self.url()).json(&request);
        if let Some(key) = self.api_key.as_ref().filter(|key| !key.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = timeout(self.sse_idle_timeout, builder.send()) => {
                result.map_err(|_| Error::Provider("模型服务非流式重试超时".into()))?
                    .map_err(|err| Error::Provider(err.to_string()))?
            }
        };
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(Error::Provider(format!("模型服务非流式重试 HTTP {status}: {detail}")));
        }
        let value: Value = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            result = timeout(self.sse_idle_timeout, response.json()) => {
                result.map_err(|_| Error::Provider("模型服务非流式重试超时".into()))?
                    .map_err(|err| Error::Provider(format!("模型服务非流式响应无法解析：{err}")))?
            }
        };
        if let Some(error) = value.get("error") {
            return Err(Error::Provider(format!("模型服务返回错误：{error}")));
        }
        let choice = value["choices"].as_array().and_then(|choices| choices.first())
            .ok_or_else(|| Error::Provider("模型服务的流式和非流式响应都没有 choices；请检查本地模型是否仍在运行，以及 OpenAI Chat Completions 兼容性".into()))?;
        let message = &choice["message"];
        let text = message["content"].as_str().unwrap_or_default().to_string();
        let mut tool_calls = Vec::new();
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                let name = call["function"]["name"].as_str().unwrap_or_default();
                if name.is_empty() { continue; }
                let tool = ToolCall {
                    id: call["id"].as_str().map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    name: name.to_string(),
                    arguments: call["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
                };
                let _ = events.send(StreamEvent::ToolCallReady(tool.clone())).await;
                tool_calls.push(tool);
            }
        }
        if !text.is_empty() {
            let _ = events.send(StreamEvent::TextDelta(text.clone())).await;
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            let reason = match reason {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::Length,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                other => FinishReason::Other(other.to_string()),
            };
            let _ = events.send(StreamEvent::Finish(reason)).await;
        }
        Ok(AssistantMessage {
            text,
            tool_calls,
            finish_reason: choice["finish_reason"].as_str().map(str::to_string),
        })
    }

    async fn attempt(
        &self,
        body: &Value,
        cancel: &CancellationToken,
        events: &mpsc::Sender<StreamEvent>,
    ) -> Result<AssistantMessage> {
        let mut builder = self
            .client
            .post(self.url())
            .header("Content-Type", "application/json")
            .json(body);
        if let Some(key) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        let send = builder.send();
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            res = send => res.map_err(|err| Error::Provider(err.to_string()))?,
        };
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            if crate::error::looks_like_context_overflow(&text) {
                return Err(Error::Provider(format!(
                    "context overflow ({status}): {text}"
                )));
            }
            return Err(Error::Provider(format!("HTTP {status}: {text}")));
        }
        let mut stream = response.bytes_stream();
        let mut buf = Vec::new();
        let mut text = String::new();
        let mut finish_reason = None;
        let mut tools: Vec<PartialTool> = Vec::new();
        let mut progressed = false;
        let mut had_choice = false;
        let mut saw_done = false;
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                chunk = timeout(self.sse_idle_timeout, stream.next()) => chunk,
            };
            match next {
                Ok(Some(Ok(bytes))) => {
                    buf.extend_from_slice(&bytes);
                    let done = drain_sse_lines(
                        &mut buf,
                        &mut text,
                        &mut finish_reason,
                        &mut tools,
                        events,
                        &mut progressed,
                        &mut had_choice,
                    )
                    .await?;
                    if done {
                        saw_done = true;
                        break;
                    }
                }
                Ok(Some(Err(err))) => {
                    return Err(stream_fail(
                        err.to_string(),
                        progressed || tools.iter().any(|tool| tool.emitted),
                    ));
                }
                Ok(None) => break,
                Err(_) => {
                    return Err(stream_fail(
                        "SSE idle timeout",
                        progressed || tools.iter().any(|tool| tool.emitted),
                    ));
                }
            }
        }
        if !saw_done {
            saw_done = drain_sse_lines(
                &mut buf,
                &mut text,
                &mut finish_reason,
                &mut tools,
                events,
                &mut progressed,
                &mut had_choice,
            )
            .await?;
        }
        if !saw_done && !buf.is_empty() {
            let leftover = String::from_utf8_lossy(&buf);
            for line in leftover.lines() {
                if apply_sse_line(
                    line,
                    &mut text,
                    &mut finish_reason,
                    &mut tools,
                    events,
                    &mut progressed,
                    &mut had_choice,
                )
                .await?
                {
                    break;
                }
            }
        }
        emit_ready_below(&mut tools, usize::MAX, events).await;
        let tool_calls: Vec<ToolCall> = tools
            .into_iter()
            .filter(|t| !t.name.is_empty() || !t.arguments.is_empty())
            .map(|mut t| {
                ensure_id(&mut t);
                t.snapshot()
            })
            .collect();
        if text.is_empty() && tool_calls.is_empty() && !had_choice {
            return Err(Error::Provider("SSE stream had no choices".into()));
        }
        Ok(AssistantMessage {
            text,
            tool_calls,
            finish_reason,
        })
    }
}

#[derive(Default)]
struct PartialTool {
    index: usize,
    id: String,
    name: String,
    arguments: String,
    emitted: bool,
    last_progress_bytes: usize,
}

impl PartialTool {
    fn snapshot(&self) -> ToolCall {
        ToolCall {
            id: self.id.clone(),
            name: self.name.clone(),
            arguments: self.arguments.clone(),
        }
    }
}

fn ensure_id(slot: &mut PartialTool) {
    if slot.id.trim().is_empty() {
        slot.id = crate::ids::ToolCallId::new().to_string();
    }
}

/// Emit `ToolCallReady` only after the call can no longer receive deltas *and*
/// its arguments are a complete JSON object. `threshold == usize::MAX` means
/// the stream (or this choice) is finished. Invalid final arguments remain in
/// the assistant message so the normal schema/error path can pair a result,
/// but must never reach early tool dispatch.
async fn emit_ready_below(
    tools: &mut [PartialTool],
    threshold: usize,
    events: &mpsc::Sender<StreamEvent>,
) {
    let mut order: Vec<usize> = tools
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            !t.emitted
                && t.index < threshold
                && !t.name.is_empty()
                && matches!(
                    serde_json::from_str::<Value>(&t.arguments),
                    Ok(Value::Object(_))
                )
        })
        .map(|(i, _)| i)
        .collect();
    order.sort_by_key(|&i| tools[i].index);
    for i in order {
        ensure_id(&mut tools[i]);
        let call = tools[i].snapshot();
        tools[i].emitted = true;
        let _ = events.send(StreamEvent::ToolCallReady(call)).await;
    }
}

#[derive(Deserialize)]
struct SseChunk {
    choices: Option<Vec<SseChoice>>,
    error: Option<Value>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: Option<SseDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct SseDelta {
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default, alias = "reasoning_content")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<SseToolCall>,
}

#[derive(Deserialize, Default)]
struct SseToolCall {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<SseFunction>,
}

#[derive(Deserialize, Default)]
struct SseFunction {
    name: Option<String>,
    arguments: Option<String>,
}

async fn drain_sse_lines(
    buf: &mut Vec<u8>,
    text: &mut String,
    finish_reason: &mut Option<String>,
    tools: &mut Vec<PartialTool>,
    events: &mpsc::Sender<StreamEvent>,
    progressed: &mut bool,
    had_choice: &mut bool,
) -> Result<bool> {
    while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
        let mut line = buf.drain(..=pos).collect::<Vec<_>>();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        let line = String::from_utf8_lossy(&line);
        if apply_sse_line(
            line.trim_end(),
            text,
            finish_reason,
            tools,
            events,
            progressed,
            had_choice,
        )
        .await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn apply_sse_line(
    line: &str,
    text: &mut String,
    finish_reason: &mut Option<String>,
    tools: &mut Vec<PartialTool>,
    events: &mpsc::Sender<StreamEvent>,
    progressed: &mut bool,
    had_choice: &mut bool,
) -> Result<bool> {
    let line = line.trim();
    if line.is_empty() || !line.starts_with("data:") {
        return Ok(false);
    }
    let data = line[5..].trim();
    if data == "[DONE]" {
        return Ok(true);
    }
    if data.is_empty() {
        return Ok(false);
    }
    let chunk: SseChunk = serde_json::from_str(data)
        .map_err(|err| Error::Provider(format!("invalid SSE json: {err}; payload={data}")))?;
    if let Some(err) = chunk.error {
        return Err(Error::Provider(format!("SSE error: {err}")));
    }
    let Some(choices) = chunk.choices else {
        return Ok(false);
    };
    if choices.is_empty() {
        return Ok(false);
    }
    *had_choice = true;
    let choice = &choices[0];
    let finishing = choice.finish_reason.is_some();
    if let Some(reason) = &choice.finish_reason {
        *finish_reason = Some(reason.clone());
    }
    if let Some(delta) = &choice.delta {
        if let Some(piece) = &delta.content {
            if !piece.is_empty() {
                *progressed = true;
                text.push_str(piece);
                let _ = events.send(StreamEvent::TextDelta(piece.clone())).await;
            }
        }
        // OpenAI-compatible local servers (including LM Studio) may stream
        // private reasoning separately from the assistant's visible content.
        // Keep the channels distinct all the way to the host UI.
        if let Some(piece) = delta.reasoning_content.as_ref().filter(|s| !s.is_empty()) {
            *progressed = true;
            let _ = events
                .send(StreamEvent::ReasoningDelta(piece.clone()))
                .await;
        }
        if let Some(piece) = delta
            .reasoning
            .as_ref()
            .filter(|s| !s.is_empty() && delta.reasoning_content.as_ref() != Some(*s))
        {
            *progressed = true;
            let _ = events
                .send(StreamEvent::ReasoningDelta(piece.clone()))
                .await;
        }
        for tc in &delta.tool_calls {
            if !tools.iter().any(|t| t.index == tc.index) {
                tools.push(PartialTool {
                    index: tc.index,
                    ..Default::default()
                });
                emit_ready_below(tools, tc.index, events).await;
            }
            let slot = tools
                .iter_mut()
                .find(|t| t.index == tc.index)
                .expect("slot exists");
            if slot.emitted {
                continue;
            }
            if let Some(id) = &tc.id {
                slot.id.push_str(id);
            }
            if let Some(func) = &tc.function {
                if let Some(name) = &func.name {
                    slot.name.push_str(name);
                }
                if let Some(args) = &func.arguments {
                    slot.arguments.push_str(args);
                    let size = slot.arguments.len();
                    if !slot.name.is_empty()
                        && size >= 256
                        && (slot.last_progress_bytes == 0
                            || size.saturating_sub(slot.last_progress_bytes) >= 1024)
                    {
                        slot.last_progress_bytes = size;
                        let _ = events
                            .send(StreamEvent::ToolCallProgress {
                                name: slot.name.clone(),
                                argument_bytes: size,
                            })
                            .await;
                    }
                }
            }
        }
    }
    if finishing {
        emit_ready_below(tools, usize::MAX, events).await;
        if let Some(reason) = finish_reason.as_deref() {
            let reason = match reason {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::Length,
                "tool_calls" | "function_call" => FinishReason::ToolCalls,
                other => FinishReason::Other(other.to_string()),
            };
            let _ = events.send(StreamEvent::Finish(reason)).await;
        }
    }
    Ok(false)
}

fn chat_to_json(m: &ChatMessage) -> Value {
    match m {
        ChatMessage::User { content, images } if images.is_empty() => {
            json!({"role":"user","content": content})
        }
        ChatMessage::User { content, images } => json!({
            "role": "user",
            "content": std::iter::once(json!({"type":"text", "text": content}))
                .chain(images.iter().map(|image| json!({"type":"image_url", "image_url":{"url":image.data_url}})))
                .collect::<Vec<_>>(),
        }),
        ChatMessage::Assistant {
            content,
            tool_calls,
        } => {
            let mut v = json!({"role":"assistant"});
            if let Some(c) = content {
                if !c.is_empty() {
                    v["content"] = json!(c);
                } else {
                    v["content"] = Value::Null;
                }
            } else {
                v["content"] = Value::Null;
            }
            if !tool_calls.is_empty() {
                v["tool_calls"] = json!(tool_calls
                    .iter()
                    .map(|t| json!({
                        "id": t.id,
                        "type": "function",
                        "function": { "name": t.name, "arguments": t.arguments }
                    }))
                    .collect::<Vec<_>>());
            }
            v
        }
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
}

fn stream_fail(detail: impl std::fmt::Display, progressed: bool) -> Error {
    if progressed {
        Error::Provider(format!("stream interrupted: {detail}"))
    } else {
        Error::Provider(detail.to_string())
    }
}

fn http_status_code(err: &Error) -> Option<u16> {
    let msg = err.to_string();
    let idx = msg.find("HTTP ")?;
    let rest = &msg[idx + 5..];
    let code = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    code.parse().ok()
}

fn retryable(err: &Error) -> bool {
    let msg = err.to_string();
    if msg.contains("stream interrupted") {
        return false;
    }
    if let Some(code) = http_status_code(err) {
        return code == 429 || (500..600).contains(&code);
    }
    msg.contains("SSE idle timeout")
        || msg.contains("error sending request")
        || msg.contains("ConnectError")
        || msg.contains("connection")
        || msg.contains("timed out")
}

fn retry_delay(base_ms: u64, cap_ms: u64, attempt: u32) -> u64 {
    let exp = base_ms.saturating_mul(1u64 << attempt.min(16));
    let capped = exp.min(cap_ms);
    let jitter_span = (capped / 4).max(1);
    let jitter = (uuid::Uuid::new_v4().as_u128() as u64) % jitter_span;
    capped.saturating_add(jitter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::provider::types::CompletionRequest;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sse_body() -> String {
        let c1 = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_ab",
                        "function": { "name": "read_file", "arguments": "{\"path\":" }
                    }]
                }
            }]
        });
        let c2 = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "\"README.md\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        format!("data: {c1}\n\ndata: {c2}\n\ndata: [DONE]\n\n")
    }

    #[test]
    fn chat_completions_capabilities_are_explicit() {
        let mut settings = Settings::defaults();
        settings.base_url = Some("http://127.0.0.1:9999/v1".into());
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        assert_eq!(provider.dialect(), ProviderDialect::OpenAiChatCompletions);
        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                parallel_tool_calls: true,
                strict_tool_schema: false,
                input_cache: false,
                resumable_stream: false,
                images: true,
            }
        );
    }

    #[tokio::test]
    async fn assembles_tool_calls_by_index() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body()),
            )
            .mount(&server)
            .await;

        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.default_model = Some("test".into());
        settings.http_connect_timeout_ms = 5_000;
        settings.sse_idle_timeout_ms = 5_000;
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let msg = provider
            .complete(
                CompletionRequest {
                    model: "test".into(),
                    temperature: 0.2,
                    max_tokens: 128,
                    reasoning_effort: Some("low".into()),
                    system: "sys".into(),
                    messages: vec![ChatMessage::User {
                        content: "hi".into(),
                        images: Vec::new(),
                    }],
                    tools: vec![],
                },
                CancellationToken::new(),
                tx,
            )
            .await
            .expect("complete");
        while rx.try_recv().is_ok() {}
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].id, "call_ab");
        assert_eq!(msg.tool_calls[0].name, "read_file");
        assert_eq!(msg.tool_calls[0].arguments, "{\"path\":\"README.md\"}");
        assert_eq!(msg.finish_reason.as_deref(), Some("tool_calls"));
    }

    fn sse_line(value: serde_json::Value) -> String {
        format!("data: {value}")
    }

    async fn apply_line(
        line: &str,
        tools: &mut Vec<PartialTool>,
        events: &mpsc::Sender<StreamEvent>,
    ) {
        let mut text = String::new();
        let mut finish = None;
        let mut progressed = false;
        let mut had_choice = false;
        apply_sse_line(
            line,
            &mut text,
            &mut finish,
            tools,
            events,
            &mut progressed,
            &mut had_choice,
        )
        .await
        .expect("sse line");
    }

    #[tokio::test]
    async fn emits_tool_call_ready_when_next_index_starts() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut tools = Vec::new();
        let c0 = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_a",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"a.txt\"}" }
                    }]
                }
            }]
        });
        apply_line(&sse_line(c0), &mut tools, &tx).await;
        assert!(
            rx.try_recv().is_err(),
            "index 0 is not ready until a higher index appears or the stream ends"
        );

        let c1 = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 1,
                        "id": "call_b",
                        "function": { "name": "read_file", "arguments": "{\"path\":\"b.txt\"}" }
                    }]
                }
            }]
        });
        apply_line(&sse_line(c1), &mut tools, &tx).await;
        match rx.try_recv().expect("first slot ready") {
            StreamEvent::ToolCallReady(call) => {
                assert_eq!(call.id, "call_a");
                assert_eq!(call.name, "read_file");
                assert_eq!(call.arguments, "{\"path\":\"a.txt\"}");
            }
            other => panic!("expected ToolCallReady, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "index 1 stays open until stream end"
        );
    }

    #[tokio::test]
    async fn long_tool_arguments_report_size_without_exposing_contents() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut tools = Vec::new();
        let secret = "private-report-body".repeat(20);
        let chunk = serde_json::json!({
            "choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call_long",
                "function": {"name": "write_file", "arguments": secret}
            }]}}]
        });
        let mut text = String::new();
        let mut finish = None;
        let mut progressed = false;
        let mut had_choice = false;
        apply_sse_line(
            &sse_line(chunk),
            &mut text,
            &mut finish,
            &mut tools,
            &tx,
            &mut progressed,
            &mut had_choice,
        )
        .await
        .unwrap();
        assert!(!progressed, "partial arguments may be retried safely");
        match rx.try_recv().expect("progress") {
            StreamEvent::ToolCallProgress {
                name,
                argument_bytes,
            } => {
                assert_eq!(name, "write_file");
                assert_eq!(argument_bytes, "private-report-body".repeat(20).len());
            }
            other => panic!("expected size-only progress, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "incomplete tool cannot dispatch");
    }

    #[tokio::test]
    async fn emits_last_tool_call_ready_on_finish_reason() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut tools = Vec::new();
        let c = serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_only",
                        "function": { "name": "read_file", "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        apply_line(&sse_line(c), &mut tools, &tx).await;
        match rx.try_recv().expect("ready on finish") {
            StreamEvent::ToolCallReady(call) => {
                assert_eq!(call.id, "call_only");
                assert_eq!(call.arguments, "{}");
            }
            other => panic!("expected ToolCallReady, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn incomplete_tool_arguments_never_reach_early_dispatch() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut tools = Vec::new();
        let invalid = serde_json::json!({
            "choices": [{
                "delta": {"tool_calls": [{
                    "index": 0, "id": "call_bad",
                    "function": {"name": "write_file", "arguments": "{\"path\":"}
                }]},
                "finish_reason": "tool_calls"
            }]
        });
        apply_line(&sse_line(invalid), &mut tools, &tx).await;
        match rx.try_recv().expect("finish event") {
            StreamEvent::Finish(FinishReason::ToolCalls) => {}
            StreamEvent::ToolCallReady(_) => {
                panic!("incomplete arguments must not be early-dispatched")
            }
            other => panic!("expected normalized finish, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emits_normalized_finish_reason() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut tools = Vec::new();
        let finished = serde_json::json!({
            "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}]
        });
        apply_line(&sse_line(finished), &mut tools, &tx).await;
        assert!(matches!(rx.recv().await, Some(StreamEvent::TextDelta(text)) if text == "done"));
        assert!(matches!(
            rx.recv().await,
            Some(StreamEvent::Finish(FinishReason::Stop))
        ));
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            model: "test".into(),
            temperature: 0.2,
            max_tokens: 128,
            reasoning_effort: Some("low".into()),
            system: "sys".into(),
            messages: vec![ChatMessage::User {
                content: "hi".into(),
                images: Vec::new(),
            }],
            tools: vec![],
        }
    }

    #[test]
    fn mlx_default_model_passes_thinking_controls_to_chat_template() {
        let mut request = req();
        request.model = "default_model".into();
        let body = OpenAiSse::body(&request);
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], "low");
        assert!(body.get("reasoning_effort").is_none());

        request.reasoning_effort = None;
        let recovery = OpenAiSse::body(&request);
        assert_eq!(recovery["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn qwen3_sends_explicit_thinking_off_when_effort_omitted() {
        // vLLM-style Qwen3 templates default thinking to ON; Off (and the
        // reasoning-only recovery) must send the template's off-switch,
        // while a configured effort keeps the generic top-level field.
        let mut request = req();
        request.model = "Qwen3.8-Flash-Next".into();
        let body = OpenAiSse::body(&request);
        assert_eq!(body["reasoning_effort"], "low");
        assert!(body.get("chat_template_kwargs").is_none());

        request.reasoning_effort = None;
        let off = OpenAiSse::body(&request);
        assert_eq!(off["chat_template_kwargs"]["enable_thinking"], false);
        assert!(off.get("reasoning_effort").is_none());
    }

    #[test]
    fn serializes_current_turn_images_as_openai_content_parts() {
        let mut request = req();
        request.messages[0] = ChatMessage::User {
            content: "hi".into(),
            images: vec![crate::provider::types::InputImage {
                data_url: "data:image/png;base64,AAAA".into(),
            }],
        };
        let body = OpenAiSse::body(&request);
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn retryable_status_codes() {
        assert!(retryable(&Error::Provider("HTTP 429: slow".into())));
        assert!(retryable(&Error::Provider("HTTP 503: down".into())));
        assert!(retryable(&Error::Provider("SSE idle timeout".into())));
        assert!(!retryable(&Error::Provider("HTTP 400: bad".into())));
        assert!(!retryable(&Error::Provider(
            "stream interrupted: SSE idle timeout".into()
        )));
        assert!(retryable(&stream_fail("SSE idle timeout", false)));
        assert!(!retryable(&stream_fail("SSE idle timeout", true)));
    }

    #[tokio::test]
    async fn skips_usage_frames_without_choices() {
        let server = MockServer::start().await;
        let usage = serde_json::json!({"usage":{"prompt_tokens":1}});
        let empty = serde_json::json!({"choices":[]});
        let text = serde_json::json!({
            "choices": [{"delta": {"content": "Hi"}, "finish_reason": "stop"}]
        });
        let body = format!("data: {usage}\n\ndata: {empty}\n\ndata: {text}\n\ndata: [DONE]\n\n");
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.default_model = Some("test".into());
        settings.http_connect_timeout_ms = 5_000;
        settings.sse_idle_timeout_ms = 5_000;
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let msg = provider
            .complete(req(), CancellationToken::new(), tx)
            .await
            .expect("complete");
        while rx.try_recv().is_ok() {}
        assert_eq!(msg.text, "Hi");
        assert_eq!(msg.finish_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn falls_back_to_nonstream_after_empty_sse() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({"stream": true})))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: {\"choices\":[]}\n\ndata: [DONE]\n\n"))
            .mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(json!({"stream": false})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "本地模型已恢复"}, "finish_reason": "stop"}]
            })))
            .mount(&server).await;
        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.default_model = Some("test".into());
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let reply = provider.complete(req(), CancellationToken::new(), tx).await.unwrap();
        assert_eq!(reply.text, "本地模型已恢复");
        assert!(matches!(rx.try_recv(), Ok(StreamEvent::TextDelta(_))));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn done_marker_stops_parsing_after_stream_end() {
        let (tx, _rx) = mpsc::channel(8);
        let mut buf = b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\ndata: invalid-after-done\n\n".to_vec();
        let mut text = String::new();
        let mut finish_reason = None;
        let mut tools = Vec::new();
        let mut progressed = false;
        let mut had_choice = false;
        let done = drain_sse_lines(
            &mut buf,
            &mut text,
            &mut finish_reason,
            &mut tools,
            &tx,
            &mut progressed,
            &mut had_choice,
        )
        .await
        .unwrap();
        assert!(done);
        assert_eq!(text, "ok");
        assert_eq!(finish_reason.as_deref(), Some("stop"));
        assert!(had_choice);
    }

    #[tokio::test]
    async fn retries_http_503_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("busy"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let text = serde_json::json!({
            "choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(format!("data: {text}\n\ndata: [DONE]\n\n")),
            )
            .mount(&server)
            .await;

        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.default_model = Some("test".into());
        settings.http_connect_timeout_ms = 5_000;
        settings.sse_idle_timeout_ms = 5_000;
        settings.retry_max = 2;
        settings.retry_base_ms = 1;
        settings.retry_cap_ms = 5;
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let msg = provider
            .complete(req(), CancellationToken::new(), tx)
            .await
            .expect("complete");
        while rx.try_recv().is_ok() {}
        assert_eq!(msg.text, "ok");
    }

    #[tokio::test]
    async fn does_not_retry_http_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
            .mount(&server)
            .await;

        let mut settings = Settings::defaults();
        settings.base_url = Some(format!("{}/v1", server.uri()));
        settings.default_model = Some("test".into());
        settings.http_connect_timeout_ms = 5_000;
        settings.sse_idle_timeout_ms = 5_000;
        settings.retry_max = 3;
        let provider = OpenAiSse::from_settings(&settings).unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let err = provider
            .complete(req(), CancellationToken::new(), tx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HTTP 400"), "{err}");
    }
}
