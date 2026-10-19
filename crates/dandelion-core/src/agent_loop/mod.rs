pub(crate) mod compact;
pub mod dispatch_plan;
mod early;
pub mod prompt;

use crate::acp::{ContentBlock, SessionUpdate, ToolCallContent, ToolCallStatus, ToolCallUpdate};
use crate::config::{PermissionMode, Settings, ThinkingIntensity};
use crate::context::ContextSession;
use crate::hooks::{HookDecision, HookEvent, HookRuntime};
use crate::host::AgentHost;
use crate::ids::{MessageId, SessionId, ToolCallId};
use crate::jobs::ProcessTable;
use crate::provider::types::{
    AssistantMessage, ChatMessage, CompletionRequest, InputImage, StreamEvent, ToolCall,
};
use crate::provider::Provider;
use crate::sandbox::WorkspaceFs;
use crate::store::{CompactTrigger, Store};
use crate::subagent::{self, SubagentRegistry, WaitAbort};
use crate::tools::{DispatchEnv, SubagentSpawner, ToolRegistry};
use crate::trace::{RunId, TraceEvent};
use crate::{Error, Result};
use dispatch_plan::{plan_segments, PlanOptions};
use early::EarlyDispatch;
use futures_util::future::join_all;
use prompt::{assemble_system_prompt_ex, AssembleOpts};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct LoopCtx {
    pub run_id: RunId,
    pub session_id: SessionId,
    pub user_text: String,
    /// Transient visual inputs belonging to the current user turn. These are
    /// retained across tool round-trips but are not persisted in SQLite.
    pub input_images: Vec<InputImage>,
    pub user_message_id: MessageId,
    pub workspace: WorkspaceFs,
    pub permission_mode: PermissionMode,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub thinking_intensity: ThinkingIntensity,
    pub settings: Arc<Settings>,
    pub store: Arc<Store>,
    pub provider: Arc<dyn Provider>,
    pub host: Arc<dyn AgentHost>,
    pub jobs: ProcessTable,
    pub tools: Arc<ToolRegistry>,
    pub cancel: CancellationToken,
    pub emit: Arc<dyn Fn(SessionUpdate) + Send + Sync>,
    pub hooks: Arc<HookRuntime>,
    pub project: String,
    /// Audited, scope-filtered P1 ledger context. It is assembled once per
    /// turn before this single loop starts; no tool or provider can expand it.
    pub context_pack: String,
    pub skills: String,
    pub parallel_unscoped: Vec<String>,
    pub context: Arc<Mutex<ContextSession>>,
    pub max_iterations: u32,
    pub depth: u32,
    pub kind: String,
    pub root_session: SessionId,
    pub identity: Option<String>,
    pub memory: String,
    pub child_goal: Option<String>,
    pub child_context: Option<String>,
    pub user_system_prompt: Option<String>,
    pub data_dir: std::path::PathBuf,
    pub subagents: Arc<SubagentRegistry>,
    pub spawner: Option<SubagentSpawner>,
    /// Set when this session uses the local sidecar GPU queue. Remote backends leave this None.
    pub local_queue: Option<Arc<crate::sidecar::LocalQueue>>,
    /// Short user-segment from sqlite-vec. Empty when unbound / maxHits=0. Never in system.
    pub corpus_user_segment: String,
    /// C16: pin write_file/patch to this directory. None = normal chat.
    pub spec_write_root: Option<std::path::PathBuf>,
    pub spec_segment: String,
    pub plan_segment: String,
}

pub async fn agent_loop(ctx: LoopCtx) -> Result<String> {
    let mut wiki_write_invoked=false;
    let initial_wiki_drafts=crate::wiki::list_drafts(&ctx.data_dir,ctx.session_id.as_str()).len();
    let emit = |u: SessionUpdate| (ctx.emit)(u);
    record_trace(
        &ctx,
        "run.started",
        serde_json::json!({
            "kind": ctx.kind,
            "depth": ctx.depth,
            "toolCount": ctx.tools.names().len(),
        }),
    );
    emit(SessionUpdate::running());
    ctx.store
        .ensure_session(&ctx.session_id, ctx.workspace.root(), &ctx.user_text)?;
    ctx.store
        .append_user(&ctx.session_id, &ctx.user_text, &ctx.user_message_id)?;
    // History keeps a viewable preview of this turn's images (files on disk,
    // paths on the row). Auxiliary: a write failure must never fail the run,
    // the model already received the transient URLs.
    let persisted_images: Vec<String> = if ctx.input_images.is_empty() {
        Vec::new()
    } else {
        let urls: Vec<String> = ctx
            .input_images
            .iter()
            .map(|image| image.data_url.clone())
            .collect();
        ctx.store
            .persist_message_images(&ctx.session_id, &ctx.user_message_id, &urls)
            .unwrap_or_default()
    };
    emit(SessionUpdate::UserMessageChunk {
        message_id: ctx.user_message_id.clone(),
        content: ContentBlock::text(&ctx.user_text),
    });
    if !persisted_images.is_empty() {
        emit(SessionUpdate::UserImages {
            message_id: ctx.user_message_id.clone(),
            files: persisted_images,
        });
    }

    let _ = ctx
        .hooks
        .fire(HookEvent::SessionStart {
            session_id: ctx.session_id.to_string(),
        })
        .await;

    let initial_names = ctx.tools.names();
    let _ = ctx
        .hooks
        .fire(HookEvent::LoadTools {
            tool_names: initial_names,
        })
        .await;
    let max_iter = ctx.max_iterations;

    let mut iterations = 0u32;
    let mut compact_attempts = 0u32;
    loop {
        if ctx.cancel.is_cancelled() {
            return finish_cancelled(&ctx).await;
        }
        if iterations >= max_iter {
            let _ = ctx.store.append_turn_note(&ctx.session_id, TURN_UNFINISHED_NOTE);
            emit(SessionUpdate::idle("max_iterations"));
            return Ok("max_iterations".into());
        }
        iterations += 1;
        record_trace(
            &ctx,
            "model.requested",
            serde_json::json!({"iteration": iterations}),
        );

        // H3: recomputed every iteration so MCP tools activated by
        // `find_tools` land in the next model request's `tools[]` and system
        // tool list, without touching the loop topology.
        let names = ctx.tools.names();
        let openai_tools = ctx.tools.openai_tools();
        let mut system = assemble_for_ctx(&ctx, &names);
        let mut messages = ctx.store.load_openai_messages(&ctx.session_id)?;
        if compact::should_preflight(
            &ctx.settings,
            ctx.max_tokens,
            &system,
            &messages,
            &openai_tools,
        ) && compact_attempts < ctx.settings.compact_max_per_prompt
            && iterations < max_iter
        {
            match compact::try_compact(&ctx, CompactTrigger::Preflight).await {
                Ok(()) => {
                    compact_attempts += 1;
                    iterations += 1;
                    system = assemble_for_ctx(&ctx, &names);
                    messages = ctx.store.load_openai_messages(&ctx.session_id)?;
                }
                Err(Error::Cancelled) => return finish_cancelled(&ctx).await,
                Err(_) => {}
            }
        }
        let assistant_id = MessageId::new();
        let early = if ctx.settings.tools_start_on_ready {
            Some(EarlyDispatch::new(ctx.clone()))
        } else {
            None
        };
        let mut saw_reasoning = false;
        let mut assistant = match complete_streaming(
            &ctx,
            system,
            messages,
            &openai_tools,
            &assistant_id,
            early.as_ref(),
            true,
            ctx.thinking_intensity.reasoning_effort(),
            &mut saw_reasoning,
        )
        .await
        {
            Ok(a) => a,
            Err(Error::Cancelled) => {
                if let Some(e) = &early {
                    e.drain_running().await;
                }
                return finish_cancelled(&ctx).await;
            }
            Err(err) if err.is_context_overflow() => {
                if let Some(e) = &early {
                    e.drain_running().await;
                }
                if ctx.settings.compact_enabled
                    && compact_attempts < ctx.settings.compact_max_per_prompt
                    && iterations + 1 < max_iter
                {
                    match compact::try_compact(&ctx, CompactTrigger::Overflow).await {
                        Ok(()) => {
                            compact_attempts += 1;
                            iterations += 1;
                            continue;
                        }
                        Err(Error::Cancelled) => return finish_cancelled(&ctx).await,
                        Err(_) => {
                            emit(SessionUpdate::idle("error"));
                            return Err(err);
                        }
                    }
                }
                emit(SessionUpdate::idle("error"));
                return Err(err);
            }
            Err(err) => {
                if let Some(e) = &early {
                    e.drain_running().await;
                }
                emit(SessionUpdate::idle("error"));
                return Err(err);
            }
        };

        if assistant.finish_reason.as_deref() == Some("length")
            && assistant.tool_calls.is_empty()
            && !assistant.text.is_empty()
            && iterations < max_iter
        {
            iterations += 1;
            assistant = match continue_once(
                &ctx,
                &openai_tools,
                assistant,
                &assistant_id,
                early.as_ref(),
            )
            .await
            {
                Ok(a) => a,
                Err(Error::Cancelled) => {
                    if let Some(e) = &early {
                        e.drain_running().await;
                    }
                    return finish_cancelled(&ctx).await;
                }
                Err(err) => {
                    if let Some(e) = &early {
                        e.drain_running().await;
                    }
                    let _ = ctx.store.append_turn_note(&ctx.session_id, TURN_UNFINISHED_NOTE);
                    emit(SessionUpdate::idle("error"));
                    return Err(err);
                }
            };
        } else if assistant.text.trim().is_empty()
            && assistant.tool_calls.is_empty()
            && saw_reasoning
            && iterations < max_iter
        {
            // Some OpenAI-compatible local servers can finish a turn after
            // streaming only reasoning_content. Give the same model one
            // visible-answer continuation, with tools still enabled.
            iterations += 1;
            let mut recovery_reasoning = false;
            assistant = match continue_after_reasoning_only(
                &ctx,
                &openai_tools,
                &assistant_id,
                early.as_ref(),
                &mut recovery_reasoning,
            )
            .await
            {
                Ok(a) => a,
                Err(Error::Cancelled) => {
                    if let Some(e) = &early {
                        e.drain_running().await;
                    }
                    return finish_cancelled(&ctx).await;
                }
                Err(err) => {
                    if let Some(e) = &early {
                        e.drain_running().await;
                    }
                    let _ = ctx.store.append_turn_note(&ctx.session_id, TURN_UNFINISHED_NOTE);
                    emit(SessionUpdate::idle("error"));
                    return Err(err);
                }
            };
        }

        if assistant.tool_calls.is_empty() && assistant.text.trim().is_empty() {
            record_trace(
                &ctx,
                "run.finished",
                serde_json::json!({"reason": "empty_visible_answer"}),
            );
            emit(SessionUpdate::idle("error"));
            return Err(Error::Provider(
                "模型未生成可见答复；请降低思考强度或增大输出 token 限额后重试".into(),
            ));
        }

        ctx.store.append_assistant(
            &ctx.session_id,
            &assistant.text,
            &assistant.tool_calls,
            &assistant_id,
        )?;
        ctx.store.tag_message_engine(&ctx.session_id, &assistant_id, "native")?;
        record_trace(
            &ctx,
            "model.completed",
            serde_json::json!({
                "iteration": iterations,
                "toolCount": assistant.tool_calls.len(),
                "finishReason": assistant.finish_reason,
            }),
        );

        wiki_write_invoked |= assistant.tool_calls.iter().any(|call|call.name=="wiki_propose");
        if assistant.tool_calls.is_empty() {
            if ctx.settings.wiki_proposal_enabled && !wiki_write_invoked && !ctx.cancel.is_cancelled()
                && (crate::wiki::request_targets_personal_knowledge(&ctx.user_text) || ctx.store.get_session(&ctx.session_id)?.is_some_and(|row|row.workspace.trim().is_empty()))
                && crate::wiki::list_drafts(&ctx.data_dir,ctx.session_id.as_str()).len()==initial_wiki_drafts {
                for draft in crate::wiki::capture_requested_reply(&ctx.store,&ctx.data_dir,ctx.session_id.as_str(),ctx.user_message_id.as_str(),&ctx.user_text,&assistant.text,ctx.settings.wiki_proposal_max_bytes)? {
                    emit(SessionUpdate::WikiDraft{draft_id:draft.draft_id,kind:draft.kind,content:draft.content,mode:if draft.knowledge_document_id.is_some(){"saved_knowledge".into()}else{draft.mode}});
                }
            }
            record_trace(
                &ctx,
                "run.finished",
                serde_json::json!({"reason": "end_turn"}),
            );
            emit(SessionUpdate::idle("end_turn"));
            return Ok("end_turn".into());
        }

        if let Some(early) = &early {
            let (mut results, cancelled) = early.collect(&assistant.tool_calls).await?;
            if !cancelled {
                results = park_if_needed(&ctx, &assistant.tool_calls, results).await;
            }
            persist_collected(&ctx, &assistant.tool_calls, &results, cancelled, early).await?;
            if cancelled {
                return finish_cancelled(&ctx).await;
            }
        } else if run_tool_batch(&ctx, &assistant.tool_calls).await? {
            return finish_cancelled(&ctx).await;
        }
    }
}

pub(super) const CANCELLED_TOOL_RESULT: &str = "Error: cancelled";

/// 轮次被用户中断的持久标记（对齐 Codex `<turn_aborted>`）。写进历史、
/// 由 `load_openai_messages` 以 user 身份送回模型，紧贴下一条用户消息：
/// 没有它，历史尾部停在半截工具调用上，模型会把下一条普通消息（哪怕
/// 只是打招呼）读成"继续干完上一轮"的信号。
pub(super) const TURN_ABORTED_NOTE: &str = "<turn_aborted>\n上一轮已被用户主动中断，没有完成；被中断的工具可能只部分执行。用户接下来的消息才是本轮唯一任务来源：除非它明确要求，不要恢复、补全或验证被中断那轮的任务。\n</turn_aborted>";

/// 出错 / 达到迭代上限退出时的同位标记：轮次同样停在半截任务上。
pub(super) const TURN_UNFINISHED_NOTE: &str = "<turn_aborted>\n上一轮没有正常结束（出错或达到迭代上限），任务可能只做了一半。用户接下来的消息才是本轮唯一任务来源：除非它明确要求，不要自动继续或补全上一轮未完成的工作。\n</turn_aborted>";

pub(super) fn plan_options(ctx: &LoopCtx) -> PlanOptions {
    // G2 C5 fills mcp__ names whose tools/list annotations.readOnlyHint is
    // true; H3 adds read-only tools activated mid-run via `find_tools`.
    let mut parallel_unscoped = ctx.parallel_unscoped.clone();
    parallel_unscoped.extend(ctx.tools.activated_parallel_unscoped());
    PlanOptions {
        enabled: ctx.settings.tools_parallel_independent,
        max: ctx.settings.tools_parallel_max.max(1),
        allow_parallel_writes: ctx.permission_mode == PermissionMode::Auto,
        parallel_unscoped,
    }
}

/// Returns `true` when the session was cancelled mid-batch.
async fn run_tool_batch(ctx: &LoopCtx, calls: &[ToolCall]) -> Result<bool> {
    let segments = plan_segments(calls, ctx.workspace.root(), &plan_options(ctx));
    let mut results: Vec<Option<String>> = vec![None; calls.len()];
    let mut next = 0usize;
    for seg in segments {
        if seg.is_empty() {
            continue;
        }
        if ctx.cancel.is_cancelled() {
            pair_cancelled_tools(ctx, calls, next)?;
            return Ok(true);
        }
        if seg.len() == 1 {
            let i = seg[0];
            results[i] = Some(dispatch_one(ctx, &calls[i]).await);
            next = i + 1;
        } else {
            let handles = seg.iter().map(|&i| {
                let ctx = ctx.clone();
                let call = calls[i].clone();
                tokio::spawn(async move { (i, dispatch_one(&ctx, &call).await) })
            });
            let joined = join_all(handles).await;
            for joined in joined {
                let (i, result) =
                    joined.map_err(|err| Error::Provider(format!("tool task: {err}")))?;
                results[i] = Some(result);
            }
            next = *seg.last().expect("non-empty") + 1;
        }
        if ctx.cancel.is_cancelled() {
            for (i, slot) in results.iter().enumerate() {
                if let Some(result) = slot {
                    ctx.store
                        .append_tool(&ctx.session_id, &calls[i].id, result)?;
                }
            }
            pair_cancelled_tools(ctx, calls, next)?;
            return Ok(true);
        }
    }
    let mut done: Vec<String> = results.into_iter().map(|r| r.expect("filled")).collect();
    done = park_if_needed(ctx, calls, done).await;
    for (call, result) in calls.iter().zip(done.iter()) {
        ctx.store.append_tool(&ctx.session_id, &call.id, result)?;
    }
    Ok(false)
}

pub(super) async fn dispatch_one(ctx: &LoopCtx, call: &ToolCall) -> String {
    record_trace(
        ctx,
        "tool.started",
        serde_json::json!({"tool": call.name, "callId": call.id}),
    );
    let env = DispatchEnv {
        session_id: ctx.session_id.clone(),
        tool_call_id: ToolCallId::parse(call.id.clone()),
        host: ctx.host.clone(),
        workspace: ctx.workspace.clone(),
        jobs: ctx.jobs.clone(),
        cancel: ctx.cancel.clone(),
        permission_mode: ctx.permission_mode,
        settings: ctx.settings.clone(),
        store: ctx.store.clone(),
        emit: ctx.emit.clone(),
        hooks: ctx.hooks.clone(),
        context: ctx.context.clone(),
        data_dir: ctx.data_dir.clone(),
        subagents: ctx.subagents.clone(),
        depth: ctx.depth,
        kind: ctx.kind.clone(),
        root_session: ctx.root_session.clone(),
        spawner: ctx.spawner.clone(),
        spec_write_root: ctx.spec_write_root.clone(),
    };
    let result = ctx.tools.dispatch(&call.name, &call.arguments, env).await;
    record_trace(
        ctx,
        "tool.finished",
        serde_json::json!({
            "tool": call.name,
            "callId": call.id,
            "ok": !result.starts_with("Error:"),
        }),
    );
    result
}

fn record_trace(ctx: &LoopCtx, kind: &str, detail: serde_json::Value) {
    let mut event = TraceEvent::new(&ctx.run_id, ctx.session_id.to_string(), kind, detail);
    if let Err(err) = ctx.store.record_trace_event(&mut event) {
        tracing::warn!(run = %ctx.run_id.0, event = kind, error = %err, "trace event was not persisted");
    }
}

fn assemble_for_ctx(ctx: &LoopCtx, names: &[String]) -> String {
    assemble_system_prompt_ex(
        ctx.workspace.root(),
        names,
        &ctx.project,
        &ctx.skills,
        AssembleOpts {
            identity: ctx.identity.as_deref(),
            memory: &ctx.memory,
            context_pack: &ctx.context_pack,
            child_goal: ctx.child_goal.as_deref(),
            child_context: ctx.child_context.as_deref(),
            user_prompt: ctx.user_system_prompt.as_deref(),
            spec: &ctx.spec_segment,
            plan: &ctx.plan_segment,
        },
    )
}

fn with_corpus_user_segment(ctx: &LoopCtx, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let extra = ctx.corpus_user_segment.trim();
    if extra.is_empty() {
        return messages;
    }
    // This segment is assembled afresh for each completion and never stored.
    // A user-supplied copy of its marker must not suppress actual retrieval.
    let insert_at = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { .. }))
        .unwrap_or(0);
    messages.insert(
        insert_at,
        ChatMessage::User {
            content: extra.to_string(),
            images: Vec::new(),
        },
    );
    messages
}

async fn park_if_needed(
    ctx: &LoopCtx,
    calls: &[ToolCall],
    mut results: Vec<String>,
) -> Vec<String> {
    if !subagent::is_park_batch(calls) {
        return results;
    }
    let interrupt = CancellationToken::new();
    ctx.subagents
        .set_park_interrupt(&ctx.session_id, interrupt.clone());
    for (i, call) in calls.iter().enumerate() {
        if subagent::delegate_action(&call.arguments) != subagent::DelegateAction::Spawn {
            continue;
        }
        if results[i].starts_with("Error:") {
            continue;
        }
        let ids = ctx
            .subagents
            .child_ids_for_tool(&ctx.session_id, &ToolCallId::parse(call.id.clone()));
        let watches: Vec<_> = ids
            .iter()
            .filter_map(|id| ctx.subagents.outcome_rx(id))
            .collect();
        if watches.is_empty() {
            continue;
        }
        match subagent::wait_children(watches, ctx.cancel.clone(), interrupt.clone()).await {
            Ok(outcomes) => {
                let parts: Vec<String> = outcomes
                    .into_iter()
                    .map(|o| {
                        o.map(|o| o.summary)
                            .unwrap_or_else(|| "Error: cancelled".into())
                    })
                    .collect();
                results[i] = parts.join("\n\n");
            }
            Err(WaitAbort::Cancelled) => {
                results[i] = CANCELLED_TOOL_RESULT.into();
            }
            Err(WaitAbort::Interrupted) => {
                // keep handle JSON so the next user prompt is paired
            }
        }
    }
    let _ = ctx.subagents.take_park_interrupt(&ctx.session_id);
    let _ = ctx.subagents.take_kicks(&ctx.session_id);
    results
}

/// Hermes `_append_cancelled_tool_results`: every assistant `tool_call` must
/// have a matching `role=tool` row, or the next OpenAI-compatible request 400s.
fn pair_cancelled_tools(ctx: &LoopCtx, calls: &[ToolCall], from: usize) -> Result<()> {
    for call in calls.get(from..).unwrap_or(&[]) {
        ctx.store
            .append_tool(&ctx.session_id, &call.id, CANCELLED_TOOL_RESULT)?;
        (ctx.emit)(SessionUpdate::tool_call(ToolCallUpdate {
            tool_call_id: ToolCallId::parse(call.id.clone()),
            title: Some(call.name.clone()),
            kind: None,
            status: Some(ToolCallStatus::Cancelled),
            raw_input: None,
            content: Some(vec![ToolCallContent::text(CANCELLED_TOOL_RESULT)]),
        }));
    }
    Ok(())
}

async fn persist_collected(
    ctx: &LoopCtx,
    calls: &[ToolCall],
    results: &[String],
    cancelled: bool,
    early: &EarlyDispatch,
) -> Result<()> {
    for (call, result) in calls.iter().zip(results.iter()) {
        ctx.store.append_tool(&ctx.session_id, &call.id, result)?;
        if cancelled && result == CANCELLED_TOOL_RESULT && !early.was_started(&call.id).await {
            (ctx.emit)(SessionUpdate::tool_call(ToolCallUpdate {
                tool_call_id: ToolCallId::parse(call.id.clone()),
                title: Some(call.name.clone()),
                kind: None,
                status: Some(ToolCallStatus::Cancelled),
                raw_input: None,
                content: Some(vec![ToolCallContent::text(CANCELLED_TOOL_RESULT)]),
            }));
        }
    }
    Ok(())
}

async fn finish_cancelled(ctx: &LoopCtx) -> Result<String> {
    ctx.jobs.kill_session(&ctx.session_id);
    // 先持久化中断标记再发 idle：客户端可能在收到 idle 后立即重读历史，
    // 标记必须在场（Codex tasks/mod.rs 同款顺序注释）。写失败不吞掉退出。
    let _ = ctx.store.append_turn_note(&ctx.session_id, TURN_ABORTED_NOTE);
    (ctx.emit)(SessionUpdate::idle("cancelled"));
    Ok("cancelled".into())
}

async fn complete_streaming(
    ctx: &LoopCtx,
    system: String,
    messages: Vec<ChatMessage>,
    tools: &[serde_json::Value],
    assistant_id: &MessageId,
    early: Option<&EarlyDispatch>,
    emit_text: bool,
    reasoning_effort: Option<String>,
    saw_reasoning: &mut bool,
) -> Result<AssistantMessage> {
    let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);
    let mut messages = with_corpus_user_segment(ctx, messages);
    if let Some(index) = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { .. }))
    {
        if let ChatMessage::User { images, .. } = &mut messages[index] {
            // SQLite intentionally stores the text transcript only. Rehydrate
            // this turn's opaque visual content before every provider call so
            // tool round-trips preserve the original multimodal user message.
            *images = ctx.input_images.clone();
        }
    }
    let req = CompletionRequest {
        model: ctx.model.clone(),
        temperature: ctx.temperature,
        max_tokens: ctx.max_tokens,
        reasoning_effort,
        system,
        messages,
        tools: tools.to_vec(),
    };
    let provider = ctx.provider.clone();
    let cancel = ctx.cancel.clone();
    if emit_text {
        match ctx
            .hooks
            .fire(HookEvent::PreLlmRequest { iteration: 0 })
            .await
        {
            HookDecision::Abort { reason } => {
                tracing::warn!(%reason, "pre_llm_request abort");
                return Err(Error::Invalid(format!("hook abort: {reason}")));
            }
            _ => {}
        }
    }
    let _lease = if let Some(queue) = &ctx.local_queue {
        let snap = queue.snapshot(ctx.settings.max_local_concurrency);
        if snap.running >= ctx.settings.max_local_concurrency as usize {
            (ctx.emit)(SessionUpdate::queued());
        }
        Some(
            queue
                .acquire(
                    ctx.session_id.clone(),
                    ctx.settings.max_local_concurrency as usize,
                    ctx.cancel.clone(),
                )
                .await?,
        )
    } else {
        None
    };
    if ctx.local_queue.is_some() {
        (ctx.emit)(SessionUpdate::running());
    }
    let join = tokio::spawn(async move { provider.complete(req, cancel, tx).await });
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::TextDelta(piece) => {
                if emit_text {
                    (ctx.emit)(SessionUpdate::AgentMessageChunk {
                        message_id: assistant_id.clone(),
                        content: ContentBlock::text(piece),
                    });
                }
            }
            StreamEvent::ToolCallReady(call) => {
                if let Some(early) = early {
                    if !ctx.cancel.is_cancelled() {
                        early.kick(call);
                    }
                }
            }
            StreamEvent::ToolCallProgress {
                name,
                argument_bytes,
            } => {
                if emit_text {
                    (ctx.emit)(SessionUpdate::ToolCallProgress {
                        name,
                        argument_bytes,
                    });
                }
            }
            StreamEvent::ReasoningDelta(piece) => {
                *saw_reasoning = true;
                if emit_text && !ctx.cancel.is_cancelled() {
                    (ctx.emit)(SessionUpdate::ReasoningDelta {
                        message_id: assistant_id.clone(),
                        text: piece,
                    });
                }
            }
            // H1a normalizes these provider-neutral events before individual
            // adapters exist. They are deliberately non-dispatching: only a
            // completed ToolCall may trigger a side effect.
            StreamEvent::Usage { .. } | StreamEvent::Finish(_) => {}
        }
    }
    join.await
        .map_err(|err| Error::Provider(format!("provider task: {err}")))?
}

async fn continue_once(
    ctx: &LoopCtx,
    tools: &[serde_json::Value],
    first: AssistantMessage,
    assistant_id: &MessageId,
    early: Option<&EarlyDispatch>,
) -> Result<AssistantMessage> {
    let mut messages = ctx.store.load_openai_messages(&ctx.session_id)?;
    messages.push(ChatMessage::Assistant {
        content: Some(first.text.clone()),
        tool_calls: Vec::new(),
    });
    messages.push(ChatMessage::User {
        content: "Continue exactly from where you stopped. No recap.".into(),
        images: Vec::new(),
    });
    let system = assemble_for_ctx(ctx, &ctx.tools.names());
    let mut saw_reasoning = false;
    match complete_streaming(
        ctx,
        system,
        messages,
        tools,
        assistant_id,
        early,
        true,
        ctx.thinking_intensity.reasoning_effort(),
        &mut saw_reasoning,
    )
    .await
    {
        Ok(mut second) => {
            second.text = format!("{}{}", first.text, second.text);
            if second.finish_reason.as_deref() == Some("length") {
                second.finish_reason = Some("stop".into());
            }
            Ok(second)
        }
        Err(Error::Cancelled) => Err(Error::Cancelled),
        Err(_) => {
            if let Some(early) = early {
                early.drain_running().await;
            }
            Ok(first)
        }
    }
}

async fn continue_after_reasoning_only(
    ctx: &LoopCtx,
    tools: &[serde_json::Value],
    assistant_id: &MessageId,
    early: Option<&EarlyDispatch>,
    saw_reasoning: &mut bool,
) -> Result<AssistantMessage> {
    let mut messages = ctx.store.load_openai_messages(&ctx.session_id)?;
    messages.push(ChatMessage::User {
        content: "上一轮只返回了推理过程，没有给用户可见的答复。请现在直接回答用户最初的问题，保留当前可用的工具能力；如需工具，正常调用；最后必须生成面向用户的正文回复。".into(),
        images: Vec::new(),
    });
    let system = assemble_for_ctx(ctx, &ctx.tools.names());
    complete_streaming(
        ctx,
        system,
        messages,
        tools,
        assistant_id,
        early,
        true,
        // Preserve the user's configured thinking on the initial request.
        // This recovery request is specifically for visible output; omitting
        // reasoning_effort lets compatible local servers switch to their
        // answer path without changing LM Studio settings.
        None,
        saw_reasoning,
    )
    .await
}
