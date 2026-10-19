//! Chat Completions analogue of Codex `OutputItemDone`: start a tool when its
//! `tool_call` slot is complete, without blocking the SSE recv loop.
//!
//! Admission reuses [`super::dispatch_plan::calls_conflict`]. Persist still
//! happens after the assistant row, in original `tool_calls` array order.

use super::dispatch_plan::{calls_conflict, PlanOptions};
use super::{dispatch_one, plan_options, LoopCtx, CANCELLED_TOOL_RESULT};
use crate::provider::types::ToolCall;
use crate::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

struct Inner {
    running: Vec<ToolCall>,
    started: HashSet<String>,
    results: HashMap<String, String>,
}

#[derive(Clone)]
pub struct EarlyDispatch {
    ctx: LoopCtx,
    opt: PlanOptions,
    inner: Arc<Mutex<Inner>>,
    gen: Arc<watch::Sender<u64>>,
    inflight: Arc<AtomicUsize>,
}

impl EarlyDispatch {
    pub fn new(ctx: LoopCtx) -> Self {
        let opt = plan_options(&ctx);
        let (tx, _rx) = watch::channel(0u64);
        Self {
            ctx,
            opt,
            inner: Arc::new(Mutex::new(Inner {
                running: Vec::new(),
                started: HashSet::new(),
                results: HashMap::new(),
            })),
            gen: Arc::new(tx),
            inflight: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn bump(&self) {
        self.gen.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Spawn admission+dispatch off the SSE recv loop (permission wait must
    /// not sit on `rx.recv`).
    pub fn kick(&self, call: ToolCall) {
        if call.name.is_empty() {
            return;
        }
        self.inflight.fetch_add(1, Ordering::SeqCst);
        let this = self.clone();
        tokio::spawn(async move {
            this.run_when_ready(call).await;
            this.inflight.fetch_sub(1, Ordering::SeqCst);
            this.bump();
        });
    }

    async fn run_when_ready(&self, call: ToolCall) {
        loop {
            let admitted = {
                let mut st = self.inner.lock().await;
                if st.results.contains_key(&call.id) || st.started.contains(&call.id) {
                    return;
                }
                if self.ctx.cancel.is_cancelled() {
                    return;
                }
                if can_admit(&st.running, &call, self.ctx.workspace.root(), &self.opt) {
                    st.started.insert(call.id.clone());
                    st.running.push(call.clone());
                    true
                } else {
                    false
                }
            };
            if admitted {
                break;
            }
            let mut rx = self.gen.subscribe();
            let admitted = {
                let mut st = self.inner.lock().await;
                if st.results.contains_key(&call.id) || st.started.contains(&call.id) {
                    return;
                }
                if self.ctx.cancel.is_cancelled() {
                    return;
                }
                if can_admit(&st.running, &call, self.ctx.workspace.root(), &self.opt) {
                    st.started.insert(call.id.clone());
                    st.running.push(call.clone());
                    true
                } else {
                    false
                }
            };
            if admitted {
                break;
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = self.ctx.cancel.cancelled() => return,
            }
        }
        let result = dispatch_one(&self.ctx, &call).await;
        {
            let mut st = self.inner.lock().await;
            st.running.retain(|r| r.id != call.id);
            st.results.insert(call.id, result);
        }
        self.bump();
    }

    /// Wait until every `call` has a result. Missing rows after cancel become
    /// `Error: cancelled`. The bool is `true` when the session cancel fired.
    pub async fn collect(&self, calls: &[ToolCall]) -> Result<(Vec<String>, bool)> {
        if !self.ctx.cancel.is_cancelled() {
            for c in calls {
                if c.name.is_empty() {
                    let mut st = self.inner.lock().await;
                    st.results
                        .entry(c.id.clone())
                        .or_insert_with(|| "Error: missing tool name".into());
                    continue;
                }
                let already = {
                    let st = self.inner.lock().await;
                    st.started.contains(&c.id) || st.results.contains_key(&c.id)
                };
                if !already {
                    self.kick(c.clone());
                }
            }
        }
        loop {
            if let Some(out) = self.try_collect(calls).await {
                return Ok(out);
            }
            let mut rx = self.gen.subscribe();
            if let Some(out) = self.try_collect(calls).await {
                return Ok(out);
            }
            if self.ctx.cancel.is_cancelled() {
                let _ = rx.changed().await;
            } else {
                tokio::select! {
                    _ = rx.changed() => {}
                    _ = self.ctx.cancel.cancelled() => {
                        self.bump();
                    }
                }
            }
        }
    }

    async fn try_collect(&self, calls: &[ToolCall]) -> Option<(Vec<String>, bool)> {
        let mut st = self.inner.lock().await;
        let all_have = calls.iter().all(|c| st.results.contains_key(&c.id));
        let cancelled = self.ctx.cancel.is_cancelled();
        if all_have {
            let out: Vec<String> = calls.iter().map(|c| st.results[&c.id].clone()).collect();
            return Some((out, cancelled));
        }
        if cancelled && st.running.is_empty() && self.inflight.load(Ordering::SeqCst) == 0 {
            let mut out = Vec::with_capacity(calls.len());
            for c in calls {
                if let Some(r) = st.results.get(&c.id) {
                    out.push(r.clone());
                } else {
                    out.push(CANCELLED_TOOL_RESULT.into());
                    st.results
                        .insert(c.id.clone(), CANCELLED_TOOL_RESULT.into());
                }
            }
            return Some((out, true));
        }
        None
    }

    pub async fn was_started(&self, id: &str) -> bool {
        self.inner.lock().await.started.contains(id)
    }

    /// Wait for spawned tasks to leave (cancel during SSE, no assistant row).
    pub async fn drain_running(&self) {
        loop {
            if self.inflight.load(Ordering::SeqCst) == 0 {
                return;
            }
            let mut rx = self.gen.subscribe();
            if self.inflight.load(Ordering::SeqCst) == 0 {
                return;
            }
            let _ = rx.changed().await;
        }
    }
}

fn can_admit(running: &[ToolCall], call: &ToolCall, workspace: &Path, opt: &PlanOptions) -> bool {
    if running.len() >= opt.max.max(1) {
        return false;
    }
    !running
        .iter()
        .any(|r| calls_conflict(r, call, workspace, opt))
}
