//! G16 (phase 7): knowledge-space attribution for Wiki writes.
//!
//! | rule | test |
//! |------|------|
//! | a space submission lands in `wiki/spaces/<id>/` and only there; each space keeps its own ledger | `space_submissions_land_in_their_own_ledgers` |
//! | malformed space ids are refused outright (never drafted, never fall back to a workspace) | `malformed_space_shape_is_refused_even_beside_a_workspace` |
//! | precedence space > workspace when both are present | `space_attribution_wins_over_a_workspace_binding` |
//! | no space, no workspace → draft (regression of the phase-0 onboarding) | same test, final block |
//! | 待归属 closed loop: unbound draft → `wiki_resolve_draft(Space)` → space ledger, session binding untouched, replay idempotent | `draft_to_space_closed_loop_and_replay` |
//! | a space that does not exist in the Store keeps the draft OPEN with a named error | `unknown_space_error_keeps_the_draft_open` |
//! | resolved records written before space support still load and replay | `pre_space_resolved_records_still_replay` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ToolCall;
use dandelion_core::wiki::{self, WikiDraftTarget};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn settings_auto() -> Settings {
    let mut s = Settings::defaults_configured();
    s.permission_mode = PermissionMode::Auto;
    s
}

fn runtime_at(ws: &std::path::Path, data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = settings_auto();
    settings.workspace = Some(ws.to_path_buf());
    fs::create_dir_all(ws).unwrap();
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

fn propose_call(id: &str, content: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "wiki_propose".into(),
        arguments: serde_json::json!({ "kind": "decision", "content": content }).to_string(),
    }
}

fn submission(kind: &str, content: &str, workspace: Option<&Path>, space_id: Option<&str>) -> wiki::WikiSubmission {
    wiki::WikiSubmission {
        kind: kind.into(),
        content: content.into(),
        mode: "propose".into(),
        revises: None,
        idempotency_key: None,
        session_id: "session-space-gate".into(),
        turn: Some(1),
        workspace: workspace.map(Path::to_path_buf),
        space_id: space_id.map(str::to_string),
    }
}

/// A tenant + space minted through the real Store API (ids are
/// `space_{uuid-simple}` — never a name or a path).
fn mint_space(rt: &Runtime, name: &str) -> String {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    rt.knowledge_create_space(&tenant.id, &customer.id, name, "")
        .unwrap()
        .id
}

#[test]
fn space_submissions_land_in_their_own_ledgers() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    // NO project, NO workspace, NO git — space attribution alone must be
    // enough for a Wiki write (work-order requirement, verbatim).
    let a = wiki::submit_wiki(&data, &submission("decision", "整单折扣不得超过五个点", None, Some("space_alpha01")), 2_000).unwrap();
    let wiki::WikiSubmitResult::Pending { entry } = a else { panic!("space submit must land pending: {a:?}") };
    assert_eq!(entry.status, wiki::ENTRY_STATUS_PENDING);
    let dir_a = wiki::space_data_dir(&data, "space_alpha01");
    let entries = wiki::load_distilled(&dir_a);
    assert_eq!(entries.len(), 1, "the entry lives in the space ledger");
    assert_eq!(entries[0].content, "整单折扣不得超过五个点");
    assert_eq!(entries[0].session_id, "session-space-gate");

    // A second space is a second, separate ledger — no cross-contamination.
    wiki::submit_wiki(&data, &submission("term", "打样单即封样基线", None, Some("space_beta02")), 2_000).unwrap();
    assert_eq!(wiki::load_distilled(&dir_a).len(), 1, "space A unchanged");
    let entries_b = wiki::load_distilled(&wiki::space_data_dir(&data, "space_beta02"));
    assert_eq!(entries_b.len(), 1);
    assert_eq!(entries_b[0].content, "打样单即封样基线");

    // Nothing went to draft on the way.
    assert!(wiki::list_drafts(&data, "session-space-gate").is_empty());
}

#[test]
fn malformed_space_shape_is_refused_even_beside_a_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    for bad in ["", " ", "../evil", "a/b", "sp ace", &"x".repeat(65)] {
        let out = wiki::submit_wiki(&data, &submission("flow", "先出样品再报价", None, Some(bad)), 2_000).unwrap();
        let wiki::WikiSubmitResult::Failed { reason } = out else {
            panic!("id {bad:?} must be refused by shape, got {out:?}");
        };
        assert!(reason.contains("形状非法"), "{bad:?} → {reason}");
        assert!(wiki::list_drafts(&data, "session-space-gate").is_empty(), "a refused submit never drafts");
    }
    // Shape check runs BEFORE the workspace fallback: a malformed space id
    // must not quietly write into the project beside it.
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let out = wiki::submit_wiki(&data, &submission("flow", "先出样品再报价", Some(&ws), Some("../evil")), 2_000).unwrap();
    assert!(matches!(out, wiki::WikiSubmitResult::Failed { .. }), "{out:?}");
    assert!(wiki::load_distilled(&wiki::resolve_project_data_dir(&data, &ws)).is_empty());
}

#[test]
fn space_attribution_wins_over_a_workspace_binding() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws).unwrap();
    let out = wiki::submit_wiki(
        &data,
        &submission("decision", "走线上审批", Some(&ws), Some("space_gamma03")),
        2_000,
    )
    .unwrap();
    assert!(matches!(out, wiki::WikiSubmitResult::Pending { .. }), "{out:?}");
    assert_eq!(wiki::load_distilled(&wiki::space_data_dir(&data, "space_gamma03")).len(), 1);
    assert!(
        wiki::load_distilled(&wiki::resolve_project_data_dir(&data, &ws)).is_empty(),
        "space identity beats a moving workspace binding"
    );

    // Regression (phase 0): no space, no workspace → durable draft, nothing
    // written anywhere.
    let out = wiki::submit_wiki(&data, &submission("pitfall", "口头改价不算数", None, None), 2_000).unwrap();
    assert!(matches!(out, wiki::WikiSubmitResult::ProjectRequired { .. }), "{out:?}");
    assert_eq!(wiki::list_drafts(&data, "session-space-gate").len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn draft_to_space_closed_loop_and_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data) = (tmp.path().join("fallback"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "报价前必须先核对客户授信额度")]),
        ScriptedTurn::text("已保存草稿"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(Some("空间归属对话")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "把这条经验记进 Wiki")).await.expect("prompt");
    let drafts = rt.wiki_drafts(&sid);
    assert_eq!(drafts.len(), 1, "unbound proposal persists as a draft");
    let draft_id = drafts[0].draft_id.clone();
    drop(drafts);

    let space_id = mint_space(&rt, "外贸报价库");
    let outcome = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id: space_id.clone() })
        .expect("space attribution resolves the draft");
    assert_eq!(outcome.status, wiki::ENTRY_STATUS_PENDING, "the draft lands as a candidate");
    assert_eq!(outcome.space_id.as_deref(), Some(space_id.as_str()));
    assert!(outcome.workspace.is_empty(), "a space is not a project: no workspace in the outcome");
    assert!(outcome.project_id.is_empty());

    // The entry sits in the space ledger — and nowhere else.
    let entries = wiki::load_distilled(&wiki::space_data_dir(&data, &space_id));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "报价前必须先核对客户授信额度");
    assert_eq!(entries[0].session_id, sid.as_str());
    assert_eq!(entries[0].id, outcome.entry_id);
    assert!(rt.wiki_entries_for(Some(&fallback)).is_empty(), "no fallback write");
    assert!(rt.projects_list().unwrap().is_empty(), "space attribution registers no project");

    // The conversation's own binding is untouched — still unbound.
    let store = dandelion_core::store::Store::open(&data, 5_000).unwrap();
    let row = store.get_session(&sid).unwrap().unwrap();
    assert!(row.workspace.trim().is_empty(), "space ≠ project: binding untouched");
    assert!(rt.wiki_drafts(&sid).is_empty(), "draft consumed");

    // Double confirmation replays the recorded outcome without writing twice.
    let again = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id: space_id.clone() })
        .expect("idempotent replay");
    assert_eq!(again.entry_id, outcome.entry_id);
    assert_eq!(again.space_id.as_deref(), Some(space_id.as_str()));
    assert_eq!(wiki::load_distilled(&wiki::space_data_dir(&data, &space_id)).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_space_error_keeps_the_draft_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data) = (tmp.path().join("fallback"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "试产通过后才允许排大货")]),
        ScriptedTurn::text("好"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(Some("空间归属对话")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "记录一下")).await.expect("prompt");
    let draft_id = rt.wiki_drafts(&sid)[0].draft_id.clone();

    let err = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id: "space_ghost_does_not_exist".into() })
        .expect_err("a Store-unknown space must be refused");
    assert!(format!("{err}").contains("所选知识空间已不存在"), "{err}");
    // The truth of a failed resolve: the draft is STILL open for a retry.
    assert_eq!(rt.wiki_drafts(&sid).len(), 1, "draft must survive a refused space");
    assert!(wiki::load_resolved_draft(&data, &draft_id).is_none());
    assert!(
        !wiki::spaces_dir(&data).join("space_ghost_does_not_exist").exists(),
        "a refused space must not get a directory"
    );

    // Retrying against a real space now lands the ORIGINAL draft.
    let space_id = mint_space(&rt, "排产规则库");
    let outcome = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id: space_id.clone() })
        .expect("retry lands");
    assert_eq!(outcome.status, wiki::ENTRY_STATUS_PENDING);
    assert_eq!(wiki::load_distilled(&wiki::space_data_dir(&data, &space_id))[0].content, "试产通过后才允许排大货");
    assert!(rt.wiki_drafts(&sid).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_space_resolved_records_still_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let (fallback, data) = (tmp.path().join("fallback"), tmp.path().join("data"));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![propose_call("p1", "对账单按自然月出具")]),
        ScriptedTurn::text("好"),
    ]));
    let rt = runtime_at(&fallback, &data, provider);
    let sid = rt.create_session(Some("兼容对话")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "记录一下")).await.expect("prompt");
    let draft_id = rt.wiki_drafts(&sid)[0].draft_id.clone();
    let space_id = mint_space(&rt, "财务规则库");
    let outcome = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id: space_id.clone() })
        .expect("resolve");

    // Downgrade the on-disk record to the PRE-space schema (drop `spaceId`):
    // every resolved draft written before phase 7 looks like this, and all
    // of them must keep loading and replaying.
    let path = data.join("wiki").join("drafts").join("resolved").join(format!("{draft_id}.json"));
    let mut raw: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(raw.get("spaceId").is_some(), "space resolutions record the attribution");
    raw.as_object_mut().unwrap().remove("spaceId");
    fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    let loaded = wiki::load_resolved_draft(&data, &draft_id).expect("legacy record still loads");
    assert_eq!(loaded.space_id, None);
    assert_eq!(loaded.entry_id, outcome.entry_id);
    let again = rt
        .wiki_resolve_draft(&draft_id, WikiDraftTarget::Space { space_id })
        .expect("legacy replay");
    assert_eq!(again.entry_id, outcome.entry_id, "replay never writes twice");
    assert_eq!(wiki::load_distilled(&wiki::space_data_dir(&data, &outcome.space_id.unwrap())).len(), 1);
}
