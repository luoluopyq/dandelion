//! `wiki_propose` — the agent's controlled write path into the project Wiki.
//!
//! Same registry / loop as the other session tools. Two intents, matching how
//! users actually ask:
//!   * `propose` (default): the agent found knowledge worth keeping. It lands
//!     in the distilled ledger as PENDING — invisible on pages until the user
//!     approves it in the Wiki panel — so the tool can never fake a "saved".
//!   * `write`: the user explicitly asked to record/update the Wiki. The call
//!     goes through gate 2 (see `permission::needs_gate2`), then the entry is
//!     accepted directly and a background recompile is queued.
//!
//! Provenance and project binding are computed server-side: the project data
//! dir derives from the session's own workspace (`ctx.workspace`) cross-checked
//! against the session row, and the ledger stores `session_id` + turn index.
//! No model-supplied path can redirect a write to another project, and the
//! ordinary file tools remain blocked from the wiki tree and `data_dir`
//! (`wiki::inference_guard`). Codex exposes the same shape
//! (`memories.add_ad_hoc_note`: schema-validated args, explicit-user-ask
//! wording); Hermes has no agent knowledge tool at all.

use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::provider::types::{ToolSource, ToolSpec};
use crate::wiki;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What the validated call is going to do — kept as a pure type so the
/// argument semantics stay unit-testable without a live ToolCtx.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WikiIntent {
    Propose,
    Write,
}

#[derive(Clone, Debug)]
pub(crate) struct WikiProposal {
    pub kind: String,
    pub content: String,
    pub intent: WikiIntent,
    pub revises: Option<String>,
}

/// Parse + validate the tool arguments. All errors are returned as model-
/// facing strings starting with "Error:" (dispatch marks the call failed on
/// that prefix).
pub(crate) fn parse_proposal(args: &Value, max_bytes: usize) -> Result<WikiProposal, String> {
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !wiki::DISTILL_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "Error: invalid kind '{kind}' (expected one of: {})",
            wiki::DISTILL_KINDS.join(", ")
        ));
    }
    let Some(raw) = args.get("content").and_then(Value::as_str) else {
        return Err("Error: content is required".into());
    };
    let content = raw.trim().to_string();
    wiki::validate_entry(&kind, &content, max_bytes).map_err(|err| format!("Error: {err}"))?;
    let intent = match args.get("mode").and_then(Value::as_str) {
        Some("write") => WikiIntent::Write,
        _ => WikiIntent::Propose,
    };
    let revises = args
        .get("revises")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(WikiProposal { kind, content, intent, revises })
}

/// Model-facing wording derived strictly from the structured outcome — the
/// same truth the UI reads. No branch here may say "已写入" for a pending,
/// duplicate, or refused submission.
pub(crate) fn reply_for(result: &wiki::WikiSubmitResult) -> String {
    match result {
        wiki::WikiSubmitResult::ProjectRequired { draft } if draft.knowledge_document_id.is_some() => format!("已保存为本地独立知识，条目 {}。无需创建项目，可在知识页面阅读和检索。",draft.knowledge_document_id.as_deref().unwrap_or_default()),
        wiki::WikiSubmitResult::ProjectRequired { .. } => "Wiki 未写入：当前对话还没有绑定项目。\
             已把这条知识保存为草稿，可以直接保存为本地独立知识，无需项目；需要项目文档时再选择项目。"
            .to_string(),
        wiki::WikiSubmitResult::Pending { entry } => format!(
            "Wiki 候选（待审核）：已记录条目 {}（{}）。用户在 Wiki 页面通过审核前，\
             它不会出现在正式页面 — 请告知用户去 Wiki 页面审核，不要声称已写入。",
            entry.id,
            wiki::entry_page(&entry.kind)
        ),
        wiki::WikiSubmitResult::Accepted { entry } => format!(
            "Wiki 已写入：条目 {}（{}）已生效，页面正在后台重新编译。",
            entry.id,
            wiki::entry_page(&entry.kind)
        ),
        wiki::WikiSubmitResult::Duplicate { existing } => format!(
            "Wiki 未写入（重复）：相同或近似内容已存在于条目 {}（状态 {}）。已向用户说明，勿重复记录。",
            existing.id, existing.status
        ),
        wiki::WikiSubmitResult::Conflict { reason } => {
            format!("Error: 未写入 Wiki — {reason}")
        }
        wiki::WikiSubmitResult::Failed { reason } => {
            format!("Error: 未写入 Wiki — {reason}")
        }
    }
}

/// The session row decides the project — never the model, and never a cwd or
/// settings fallback. `Ok(Some(ws))`: bound and consistent with the sandbox.
/// `Ok(None)`: the session has NO bound project (wiki must not write into a
/// directory the session did not choose) → the caller persists a draft and
/// offers the onboarding flow. `Err`: unknown session or a project switch
/// mid-flight (row moved away from the running workspace) → refuse.
fn session_project(ctx: &ToolCtx, workspace_root: &Path) -> Result<Option<PathBuf>, String> {
    let canonical = |p: &Path| dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match ctx.store.get_session(&ctx.session_id) {
        Ok(Some(row)) => {
            let raw = row.workspace.trim().to_string();
            if raw.is_empty() {
                return Ok(None);
            }
            let bound = PathBuf::from(&raw);
            if canonical(&bound) != canonical(workspace_root) {
                return Err(format!(
                    "Error: this session is bound to {} (not the current workspace {}); \
                     wiki write refused",
                    bound.display(),
                    workspace_root.display()
                ));
            }
            Ok(Some(bound))
        }
        Ok(None) => Err("Error: unknown session; wiki write refused".into()),
        Err(err) => Err(format!("Error: cannot verify session project: {err}")),
    }
}

/// Best-effort turn locator: count the session's user messages. A failure here
/// must not fail the knowledge write, only drop the locator.
fn current_turn(ctx: &ToolCtx) -> Option<u32> {
    let export = ctx
        .store
        .session_export_window(&ctx.session_id, 0, u32::MAX)
        .ok()?;
    let count = export["messages"]
        .as_array()?
        .iter()
        .filter(|m| m["role"].as_str() == Some("user"))
        .count();
    u32::try_from(count).ok()
}

/// Queue the background recompile (coalesced per project, never blocking this
/// turn) and report it in the tool text so the UI can show the real state.
fn queue_compile(ctx: &ToolCtx, project_data: PathBuf, workspace: PathBuf) {
    let emit = ctx.emit.clone();
    let ws_str = workspace.to_string_lossy().into_owned();
    let notify: wiki::WikiNotifier = std::sync::Arc::new(move |status, version, error| {
        emit(crate::acp::SessionUpdate::wiki_update(
            status, version, error, Some(ws_str.clone()),
        ));
    });
    wiki::schedule_compile(
        project_data,
        workspace,
        ctx.settings.wiki_max_scan_files,
        ctx.settings.wiki_max_file_bytes,
        Some(crate::wiki_index::IndexBudget::from_settings(&ctx.settings)),
        notify,
    );
}

pub struct WikiProposeTool;

impl WikiProposeTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WikiProposeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WikiProposeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "wiki_propose".into(),
            description: "Record durable project knowledge into the current project's Wiki \
(flow = business/process flow, decision = an adopted decision, pitfall = a trap and how to \
avoid it, term = project-specific terminology). Two modes: \"propose\" (default) files the \
entry as PENDING for the user to review in the Wiki page — until approved it is NOT part of \
the wiki and you must not tell the user it was written; \"write\" is only for when the user \
explicitly asked to save/update this knowledge in the Wiki, and it needs the user's approval. \
Pass revises=<entry id> to correct an entry you or a distill already recorded instead of \
adding a near-duplicate. One self-contained item per call, ≤2000 bytes, grounded in this \
conversation. The wiki belongs to the current project; there is no path argument and file \
tools must never touch wiki data."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["flow", "decision", "pitfall", "term"],
                        "description": "Knowledge type. Required."
                    },
                    "content": {
                        "type": "string",
                        "description": "One self-contained statement, ≤2000 bytes."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["propose", "write"],
                        "description": "propose: pending review (default). write: only when the user explicitly asked to record this in the Wiki."
                    },
                    "revises": {
                        "type": "string",
                        "description": "Optional existing entry id this proposal corrects."
                    }
                },
                "required": ["kind", "content"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let proposal = match parse_proposal(&args, ctx.settings.wiki_proposal_max_bytes) {
            Ok(p) => p,
            Err(err) => return err,
        };
        let personal=wiki::request_targets_personal_knowledge(&ctx.store.knowledge_latest_request(&ctx.session_id).unwrap_or_default());
        let workspace_root = ctx.workspace.root().to_path_buf();
        let workspace = match session_project(&ctx, &workspace_root) {
            Ok(Some(ws)) if !personal => Some(ws),
            // No bound project: never fall back to cwd/settings — persist the
            // draft on the session and let the desktop offer the onboarding
            // flow. Nothing entered the ledger, so nothing may be claimed.
            Ok(_) => {
                let submission = wiki::WikiSubmission {
                    kind: proposal.kind.clone(),
                    content: proposal.content.clone(),
                    mode: match proposal.intent {
                        WikiIntent::Propose => "propose".to_string(),
                        WikiIntent::Write => "write".to_string(),
                    },
                    revises: proposal.revises.clone(),
                    // A dispatch-level retry re-runs the same tool call id;
                    // the key makes the second landing a duplicate, not a
                    // second draft or entry.
                    idempotency_key: Some(format!("native:{}", ctx.tool_call_id.as_str())),
                    session_id: ctx.session_id.as_str().to_string(),
                    turn: current_turn(&ctx),
                    workspace: None,
                    space_id: None,
                };
                let result = match wiki::submit_wiki(&ctx.data_dir, &submission, ctx.settings.wiki_proposal_max_bytes) {
                    Ok(result) => result,
                    Err(err) => return format!("Error: wiki draft failed: {err}"),
                };
                let mut result=result;
                if let Err(error)=wiki::save_unbound_knowledge(&ctx.store,&ctx.data_dir,&mut result){return format!("Error: 本地知识保存失败：{error}");}
                if let wiki::WikiSubmitResult::ProjectRequired { draft } = &result {
                    (ctx.emit)(crate::acp::SessionUpdate::WikiDraft {
                        draft_id: draft.draft_id.clone(),
                        kind: draft.kind.clone(),
                        content: draft.content.clone(),
                        mode: if draft.knowledge_document_id.is_some(){"saved_knowledge".into()}else{draft.mode.clone()},
                    });
                }
                return reply_for(&result);
            }
            Err(err) => return err,
        };
        // The model cannot choose the project: it is the session's own bound
        // workspace (verified above), hashed into the project wiki dir by core.
        if let Some(ws) = &workspace {
            if let Err(error) = crate::wiki_space_migrate::ensure_space_dir(&ctx.store,&ctx.data_dir,ws,ctx.settings.wiki_auto_migrate) {
                return format!("Error: 项目知识目录尚未准备完成：{error}");
            }
        }
        let submission = wiki::WikiSubmission {
            kind: proposal.kind.clone(),
            content: proposal.content.clone(),
            mode: match proposal.intent {
                WikiIntent::Propose => "propose".to_string(),
                WikiIntent::Write => "write".to_string(),
            },
            revises: proposal.revises.clone(),
            idempotency_key: Some(format!("native:{}", ctx.tool_call_id.as_str())),
            session_id: ctx.session_id.as_str().to_string(),
            turn: current_turn(&ctx),
            workspace,
            space_id: None,
        };
        let result = match wiki::submit_wiki(&ctx.data_dir, &submission, ctx.settings.wiki_proposal_max_bytes) {
            Ok(result) => result,
            Err(err) => return format!("Error: wiki write failed: {err}"),
        };
        // Persisted for sure before we claim anything or queue the recompile.
        if let Some(ws) = &submission.workspace {
            if matches!(
                result,
                wiki::WikiSubmitResult::Pending { .. } | wiki::WikiSubmitResult::Accepted { .. }
            ) {
                queue_compile(&ctx, wiki::resolve_project_data_dir(&ctx.data_dir, ws), ws.clone());
            }
        }
        reply_for(&result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(kind: &str, content: &str, mode: Option<&str>) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("kind".into(), json!(kind));
        map.insert("content".into(), json!(content));
        if let Some(m) = mode {
            map.insert("mode".into(), json!(m));
        }
        Value::Object(map)
    }

    #[test]
    fn parse_accepts_the_two_intents_and_defaults_to_propose() {
        let p = parse_proposal(&args("flow", "先出样品再报价", None), 2_000).unwrap();
        assert_eq!(p.intent, WikiIntent::Propose);
        let w = parse_proposal(&args("decision", "走线上审批", Some("write")), 2_000).unwrap();
        assert_eq!(w.intent, WikiIntent::Write);
        assert_eq!(w.kind, "decision");
    }

    #[test]
    fn parse_rejects_bad_kind_empty_and_oversized_content() {
        assert!(parse_proposal(&args("opinion", "x", None), 2_000)
            .unwrap_err()
            .contains("kind"));
        assert!(parse_proposal(&args("flow", "  ", None), 2_000)
            .unwrap_err()
            .starts_with("Error:"));
        let big = "知".repeat(1000); // 3000 bytes > 2000 cap
        assert!(parse_proposal(&args("term", &big, None), 2_000)
            .unwrap_err()
            .contains("超长"));
    }

    #[test]
    fn revises_is_trimmed_and_blank_means_new_entry() {
        let mut a = args("flow", "x", None);
        a.as_object_mut()
            .unwrap()
            .insert("revises".into(), json!("  "));
        assert_eq!(parse_proposal(&a, 2_000).unwrap().revises, None);
        a.as_object_mut()
            .unwrap()
            .insert("revises".into(), json!(" id-1 "));
        assert_eq!(
            parse_proposal(&a, 2_000).unwrap().revises.as_deref(),
            Some("id-1")
        );
    }
}
