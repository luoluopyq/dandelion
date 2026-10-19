//! G18 (phase 8): the unified knowledge-space read surface.
//!
//! | rule | test |
//! |------|------|
//! | one unreadable space fails the WHOLE multi-space call (fail closed, zero leaked hits); the readable subset answers alone | `unreadable_space_fails_the_whole_call` |
//! | ghost ids and archived spaces are refused by NAME; an empty id list is refused | `unknown_and_archived_spaces_are_named` |
//! | a `local_only` space answers with `allow_cloud=true` WITHOUT a model turn (extractive, honest policy note) | `local_only_space_answers_without_the_model` |
//! | `allow_summary` answers via the model; the SAME runtime mixed with one `local_only` space degrades to extract | `allow_summary_space_uses_the_model_but_a_local_only_space_blocks_the_pack` |
//! | article + chunk + vector surfaces merge over one explicit space list | `three_surfaces_merge_over_one_space_list` |
//! | KNN neighbours that share no term with the question never count as evidence → honest no-answer | `irrelevant_knn_neighbours_are_not_evidence` |
//! | article BROWSING (`space_status`/`space_show`) runs behind the SAME Reader gate; archived spaces refuse by name | `article_browsing_shares_the_read_gate` |
//! | space-ledger review (`space_entries`/`space_review_entry`/`space_edit_entry`) runs behind the same gate — Reader lists, Editor mutates — with the workspace trio's ledger semantics; a pure space never queues the idle compile and does not error for it | `space_ledger_review_walks_the_gate` |
//! | a workspace-bound space's ledger IS the workspace ledger — space-keyed approve shows through the workspace-keyed read and vice versa | `a_bound_space_ledger_is_the_workspace_ledger` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki;
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use dandelion_core::{KnowledgeRole, PermissionMode, Runtime, Settings};
use std::fs;
use std::sync::Arc;

fn runtime_at(
    ws: &std::path::Path,
    data: &std::path::Path,
    provider: Arc<ScriptedProvider>,
) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(ws.to_path_buf());
    fs::create_dir_all(ws).unwrap();
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

/// Tenant + customer + space minted through the real Store API (ids are
/// generated, never names or paths).
fn mint(rt: &Runtime, name: &str, policy: &str) -> (String, String, String) {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let space = rt.knowledge_create_space(&tenant.id, &customer.id, name, policy).unwrap();
    (tenant.id, customer.id, space.id)
}

/// Owner-member + one published document, so lexical chunk hits exist.
fn seed(rt: &Runtime, t: &str, c: &str, s: &str, subject: &str, content: &str) {
    rt.knowledge_set_member(s, subject, KnowledgeRole::Owner).unwrap();
    let (doc, version) = rt
        .knowledge_import_document(t, c, s, subject, "note", "notes.md", content, 500)
        .unwrap();
    rt.knowledge_publish_version(t, c, s, subject, &doc.id, &version.id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_space_fails_the_whole_call() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (ta, ca, a) = mint(&rt, "可读书目库", "");
    let (tb, cb, b) = mint(&rt, "机密合同库", "");
    seed(&rt, &ta, &ca, &a, "tester", "整单折扣超过五个点须总监签字。");
    seed(&rt, &tb, &cb, &b, "boss", "暗中汇率条款由甲方单方解释。");

    // B has NO membership for "tester" — the whole call fails, even though
    // A alone resolves fine, and even when the QUERY only names B's secret.
    for q in ["整单折扣", "暗中汇率条款"] {
        let err = rt
            .space_search("tester", &[a.clone(), b.clone()], q, 10)
            .expect_err("an unreadable space must fail the whole call");
        let msg = err.to_string();
        assert!(msg.contains("knowledge access denied"), "{msg}");
        assert!(!msg.contains("暗中汇率"), "the denial never echoes the secret: {msg}");
    }

    // The readable subset answers ALONE.
    let ok = rt.space_search("tester", std::slice::from_ref(&a), "整单折扣", 10).unwrap();
    assert!(!ok.hits.is_empty(), "A's published chunk must answer");
    assert!(ok.hits.iter().all(|h| h.space_id == a), "only A may appear");
    assert!(ok.hits.iter().any(|h| h.surface == "chunk" && h.excerpt.contains("整单折扣")));
    assert_eq!(ok.empty_spaces.len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_and_archived_spaces_are_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));

    let err = rt
        .space_search("tester", &["space_ghost000".into()], "任意问题", 10)
        .expect_err("a ghost id is refused");
    assert!(err.to_string().contains("知识空间不存在"), "{err}");

    let (_t, _c, s) = mint(&rt, "将被归档的空间", "");
    rt.knowledge_set_member(&s, "tester", KnowledgeRole::Reader).unwrap();
    rt.knowledge_archive_space(&s, true).unwrap();
    let err = rt
        .space_search("tester", std::slice::from_ref(&s), "任意问题", 10)
        .expect_err("an archived space is refused by name");
    assert!(err.to_string().contains("已归档"), "{err}");

    let err = rt.space_search("tester", &[], "任意问题", 10).expect_err("empty list refused");
    assert!(err.to_string().contains("必须显式指定"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_only_space_answers_without_the_model() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    // ZERO scripted turns: any model call would fail outright — the honest
    // proof below is that we never needed one AND say why.
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "车间工艺库", ""); // default policy: local_only
    seed(&rt, &t, &c, &s, "tester", "保压时间不足会出现缩痕。");

    let ans = rt
        .space_ask("tester", std::slice::from_ref(&s), "保压时间不足会怎样", 5, true)
        .await
        .expect("local_only still answers, extractively");
    assert_eq!(ans.mode, "local_extract");
    assert!(!ans.no_answer);
    assert!(ans.answer.contains("缩痕"), "{}", ans.answer);
    assert!(!ans.sources.is_empty(), "citations ride the extract");
    let note = ans.policy_note.expect("the block is reported honestly");
    assert!(note.contains("云端摘要") && note.contains("改用本机摘录"), "{note}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allow_summary_space_uses_the_model_but_a_local_only_space_blocks_the_pack() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    // TWO turns staged: if the mixed call wrongly reached the model it WOULD
    // succeed as cloud_summary — the assertion failing is the detection.
    let rt = runtime_at(
        &ws,
        &data,
        Arc::new(ScriptedProvider::new(vec![
            ScriptedTurn::text("云端摘要：保压不足会出现缩痕 [1]。"),
            ScriptedTurn::text("越权摘要：这不该发生。"),
        ])),
    );
    let (t, c, e) = mint(&rt, "可摘要知识库", "allow_summary");
    seed(&rt, &t, &c, &e, "tester", "保压时间不足会出现缩痕。");
    let ans = rt
        .space_ask("tester", std::slice::from_ref(&e), "保压时间不足会怎样", 5, true)
        .await
        .expect("policy allows the summary");
    assert_eq!(ans.mode, "cloud_summary", "note={:?} answer={}", ans.policy_note, ans.answer);
    assert!(ans.answer.contains("云端摘要"), "{}", ans.answer);
    assert!(!ans.sources.is_empty());
    assert!(ans.policy_note.is_none());

    // ONE local_only space in the list blocks the WHOLE cloud turn.
    let (t2, c2, d) = mint(&rt, "本机摘录库", "");
    seed(&rt, &t2, &c2, &d, "tester", "模具温度波动导致缩水。");
    let mixed = rt
        .space_ask("tester", &[e.clone(), d.clone()], "保压时间不足会怎样", 5, true)
        .await
        .expect("degrades, never errors");
    assert_eq!(mixed.mode, "local_extract", "one local_only space blocks the pack");
    assert!(!mixed.answer.contains("越权摘要"), "the staged turn must stay unconsumed");
    assert!(mixed.policy_note.expect("honest note").contains("云端摘要"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_surfaces_merge_over_one_space_list() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));

    // (vector + chunk) corpus space, provisioned by a real import.
    let src = tmp.path().join("冷却塔.md");
    fs::write(&src, "# 冷却塔\n\n填料更换周期为两年，每季度检查布水器。\n").unwrap();
    rt.corpus_import(&src).unwrap();
    let corpus_id = rt.corpus_space_id().unwrap().expect("import provisions the corpus space");

    // (article) a Wiki space with a published article, no project anywhere.
    let docs = tmp.path().join("docs-home");
    fs::create_dir_all(docs.join("docs")).unwrap();
    fs::write(docs.join("docs/notes.md"), "# 工艺笔记\n\n模具温度波动会导致缩水。\n").unwrap();
    let (_, _, w) = mint(&rt, "注塑知识库", "");
    rt.knowledge_set_member(&w, "local-user", KnowledgeRole::Owner).unwrap();
    let space_dir = wiki::space_data_dir(&data, &w);
    let idx = wi::build_index(&space_dir, &docs, 100, 65_536).unwrap();
    wb::publish_build(
        &space_dir,
        &idx,
        &[wb::StagedArticle {
            file: "TOPIC-注塑.md".into(),
            title: "注塑缺陷".into(),
            focus: String::new(),
            content: "模具温度波动会导致缩水，成因见 `docs/notes.md:L1-L3`。\n".into(),
            inputs: vec!["docs/notes.md".into()],
            ranges: Vec::new(),
        }],
    )
    .unwrap();

    // One call, both spaces: the corpus question answers from the corpus
    // space (chunk and/or vector), the article question from the Wiki space.
    let both = vec![corpus_id.clone(), w.clone()];
    let cooling = rt.space_search("local-user", &both, "冷却塔填料", 10).unwrap();
    assert!(
        cooling.hits.iter().any(|h| h.space_id == corpus_id
            && (h.surface == "vector" || h.surface == "chunk")
            && h.excerpt.contains("填料")),
        "corpus surfaces must answer: {:?}",
        cooling.hits.iter().map(|h| (&h.surface, &h.label)).collect::<Vec<_>>()
    );
    let shrink = rt.space_search("local-user", &both, "模具温度", 10).unwrap();
    assert!(
        shrink.hits.iter().any(|h| h.space_id == w && h.surface == "article"),
        "the article surface must answer: {:?}",
        shrink.hits.iter().map(|h| (&h.surface, &h.label)).collect::<Vec<_>>()
    );
    // The space with no match is ENUMERATED, not silently dropped.
    assert!(cooling.empty_spaces.iter().any(|e| e.space_id == w));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn irrelevant_knn_neighbours_are_not_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));
    let src = tmp.path().join("冷却塔.md");
    fs::write(&src, "# 冷却塔\n\n填料更换周期为两年。\n").unwrap();
    rt.corpus_import(&src).unwrap();
    let corpus_id = rt.corpus_space_id().unwrap().expect("corpus space");

    // KNN ALWAYS returns neighbours; without the lexical-overlap guard this
    // question about rent on Mars would "find" cooling-tower chunks.
    let ans = rt
        .space_ask("local-user", std::slice::from_ref(&corpus_id), "火星殖民地的租金如何缴纳", 4, false)
        .await
        .unwrap();
    assert!(ans.no_answer, "unrelated KNN neighbours must not fabricate evidence: {:?}", ans.sources);
    assert!(ans.sources.is_empty());
    assert!(ans.mode == "local_extract");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn article_browsing_shares_the_read_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));

    // A published article in a workspace-less space: reachable ONLY through
    // the space id, never through wiki_* (which key on a workspace path).
    let docs = tmp.path().join("docs-home");
    fs::create_dir_all(docs.join("docs")).unwrap();
    fs::write(docs.join("docs/notes.md"), "# 工艺笔记\n\n模具温度波动会导致缩水。\n").unwrap();
    let (_, _, w) = mint(&rt, "注塑知识库", "");
    rt.knowledge_set_member(&w, "local-user", KnowledgeRole::Owner).unwrap();
    let space_dir = wiki::space_data_dir(&data, &w);
    let idx = wi::build_index(&space_dir, &docs, 100, 65_536).unwrap();
    wb::publish_build(
        &space_dir,
        &idx,
        &[wb::StagedArticle {
            file: "TOPIC-注塑.md".into(),
            title: "注塑缺陷".into(),
            focus: String::new(),
            content: "模具温度波动会导致缩水，成因见 `docs/notes.md:L1-L3`。\n".into(),
            inputs: vec!["docs/notes.md".into()],
            ranges: Vec::new(),
        }],
    )
    .unwrap();

    // Member browsing: the TOC lists the article, the body reads back.
    let status = rt.space_status("local-user", &w).expect("Owner may browse");
    assert!(
        status.pages.iter().any(|p| p.file == "TOPIC-注塑.md"),
        "published article must appear in the TOC: {:?}",
        status.pages.iter().map(|p| &p.file).collect::<Vec<_>>()
    );
    let one = rt.space_show("local-user", &w, Some("TOPIC-注塑.md")).expect("article reads");
    assert_eq!(one.len(), 1);
    assert!(one[0].body.contains("缩水"), "{}", one[0].body);
    let all = rt.space_show("local-user", &w, None).expect("every page reads");
    assert!(all.iter().any(|p| p.body.contains("缩水")));

    // Non-member: the SAME fail-closed denial as space_search, and it never
    // echoes the article content back.
    for res in [
        rt.space_status("ghost", &w).map(|_| ()),
        rt.space_show("ghost", &w, None).map(|_| ()),
    ] {
        let err = res.expect_err("article browsing needs Reader");
        let msg = err.to_string();
        assert!(msg.contains("knowledge access denied"), "{msg}");
        assert!(!msg.contains("缩水"), "the denial never echoes the article: {msg}");
    }

    // Archived: refused BY NAME, same as the search surface.
    rt.knowledge_archive_space(&w, true).unwrap();
    let err = rt
        .space_status("local-user", &w)
        .expect_err("an archived space is refused");
    assert!(err.to_string().contains("已归档"), "{err}");
    let err = rt
        .space_show("local-user", &w, Some("TOPIC-注塑.md"))
        .expect_err("an archived space is refused");
    assert!(err.to_string().contains("已归档"), "{err}");
}

/// A space nothing was ever built into is an honest EMPTY read, not an
/// error. The workspace-keyed `wiki::show` answer ("wiki not compiled yet;
/// run dln wiki compile") is CLI advice; leaking it through `space_show`
/// would paint a red alert on every fresh space in the GUI. The read gate
/// still runs BEFORE the empty short-circuit — a ghost is denied exactly as
/// on a built space.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_built_space_shows_empty_not_a_cli_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (_, _, w) = mint(&rt, "从未构建的空间", "");
    rt.knowledge_set_member(&w, "local-user", KnowledgeRole::Owner).unwrap();

    let status = rt.space_status("local-user", &w).expect("empty space has an empty status");
    assert!(status.pages.is_empty());
    let msg = |r: &dandelion_core::Result<Vec<wiki::WikiPage>>| r.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    let all = rt.space_show("local-user", &w, None);
    assert!(all.as_ref().is_ok(), "never-built space reads EMPTY, not the CLI hint: {}", msg(&all));
    assert!(all.unwrap().is_empty());
    let one = rt.space_show("local-user", &w, Some("不存在的页.md"));
    assert!(one.as_ref().is_ok(), "page miss on an unbuilt tree is EMPTY: {}", msg(&one));
    assert!(one.unwrap().is_empty());

    let ghost = rt.space_show("ghost", &w, None).expect_err("the gate runs before the empty shortcut");
    assert!(ghost.to_string().contains("knowledge access denied"), "{ghost}");
}

/// Space-ledger review (acc05's recorded gap, now closed): `submit_wiki`'s
/// space attribution lands proposals in `wiki/spaces/<id>/wiki/distilled.jsonl`,
/// and `space_entries`/`space_review_entry`/`space_edit_entry` are the
/// space-keyed review entry points — same fail-closed gate as every other
/// space read (ghost named, archived named, Reader lists, Editor mutates),
/// with the workspace trio's exact ledger semantics (approve/reject/undo,
/// "unknown action"/"entry not found" wording). A pure space never touches
/// the idle-compile pipeline and must not error because of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_ledger_review_walks_the_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (_, _, s) = mint(&rt, "报价规则空间", "");
    rt.knowledge_set_member(&s, "local-user", KnowledgeRole::Owner).unwrap();
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    rt.knowledge_set_member(&s, "editor1", KnowledgeRole::Editor).unwrap();

    // A proposal through the REAL submit path (mode=propose → pending).
    let sub = wiki::WikiSubmission {
        kind: "decision".into(),
        content: "整单折扣超过五个点须总监签字。".into(),
        mode: "propose".into(),
        revises: None,
        idempotency_key: None,
        session_id: "session-ledger".into(),
        turn: Some(3),
        workspace: None,
        space_id: Some(s.clone()),
    };
    let out = wiki::submit_wiki(&data, &sub, 2_000).unwrap();
    let entry_id = out.entry_id().expect("pending entry").to_string();

    // Non-member: denied like every other space read, and the denial never
    // echoes the pending content.
    let err = rt.space_entries("ghost", &s).expect_err("listing needs membership");
    let msg = err.to_string();
    assert!(msg.contains("knowledge access denied"), "{msg}");
    assert!(!msg.contains("总监签字"), "the denial never echoes the proposal: {msg}");

    // Reader may list but not mutate; Editor may mutate.
    let listed = rt.space_entries("reader1", &s).expect("Reader lists the ledger");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "pending");
    for res in [
        rt.space_review_entry("reader1", &s, &entry_id, "approve"),
        rt.space_edit_entry("reader1", &s, &entry_id, Some("改"), None),
    ] {
        let err = res.expect_err("review and edit need Editor");
        assert!(err.to_string().contains("role does not permit"), "{err}");
    }
    assert_eq!(
        rt.space_entries("reader1", &s).unwrap()[0].status,
        "pending",
        "the refused approve changed nothing"
    );

    // The action words and messages are the workspace trio's, verbatim.
    let err = rt
        .space_review_entry("editor1", &s, &entry_id, "dance")
        .expect_err("unknown action is named");
    assert!(err.to_string().contains("unknown wiki entry action: dance"), "{err}");
    let err = rt
        .space_review_entry("editor1", &s, "no_such_entry", "approve")
        .expect_err("unknown id is named");
    assert!(err.to_string().contains("wiki entry not found: no_such_entry"), "{err}");

    rt.space_review_entry("editor1", &s, &entry_id, "approve").expect("Editor approves");
    assert_eq!(rt.space_entries("reader1", &s).unwrap()[0].status, "accepted");
    rt.space_edit_entry("editor1", &s, &entry_id, Some("整单折扣超过五个点须总监书面签字。"), Some("flow"))
        .expect("Editor edits");
    let edited = &rt.space_entries("reader1", &s).unwrap()[0];
    assert!(edited.content.contains("书面"));
    assert_eq!(edited.kind, "flow");
    rt.space_review_entry("editor1", &s, &entry_id, "undo").expect("undo removes");
    assert!(rt.space_entries("reader1", &s).unwrap().is_empty(), "undo removes the entry");

    // Identity and lifecycle wording match the read surfaces exactly.
    let err = rt.space_entries("local-user", "space_ghost000").expect_err("ghost named");
    assert!(err.to_string().contains("知识空间不存在"), "{err}");
    let err = rt.space_entries("local-user", "../evil").expect_err("a malformed id never touches disk");
    assert!(err.to_string().contains("形状非法"), "{err}");
    rt.knowledge_archive_space(&s, true).unwrap();
    let err = rt.space_entries("local-user", &s).expect_err("archived named");
    assert!(err.to_string().contains("已归档"), "{err}");
}

/// A workspace-bound space's ledger IS its project ledger (phase 2 moved
/// project data into `wiki/spaces/<id>`), so a space-keyed approve must be
/// visible through the workspace-keyed read — one ledger, two entry points.
/// A pure space keeps its ledger honestly too; neither path errors over
/// the pure space having no idle-compile pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_space_ledger_is_the_workspace_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, data) = (tmp.path().join("ws"), tmp.path().join("data"));
    let rt = runtime_at(&ws, &data, Arc::new(ScriptedProvider::new(Vec::new())));

    // Resolving the workspace creates + registers its wiki space
    // (LOCAL_SUBJECT = "local-user" owns it).
    let dir = rt.wiki_dir_for(Some(&ws));
    let sid = dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(wiki::is_safe_space_id(&sid), "{sid}");
    assert_eq!(dir, wiki::space_data_dir(&data, &sid));

    let sub = wiki::WikiSubmission {
        kind: "term".into(),
        content: "封样单即打样基线。".into(),
        mode: "propose".into(),
        revises: None,
        idempotency_key: None,
        session_id: "session-bound".into(),
        turn: Some(1),
        workspace: Some(ws.clone()),
        space_id: Some(sid.clone()),
    };
    let out = wiki::submit_wiki(&data, &sub, 2_000).unwrap();
    let entry_id = out.entry_id().expect("pending entry").to_string();

    // Space-keyed approve…
    rt.space_review_entry("local-user", &sid, &entry_id, "approve").expect("Owner approves via space");
    // …shows through the workspace-keyed read: the same ledger.
    let via_ws = rt.wiki_entries_for(Some(&ws));
    assert_eq!(via_ws.iter().find(|e| e.id == entry_id).map(|e| e.status.as_str()), Some("accepted"));
    // And the reverse direction stays true for a workspace-keyed undo.
    rt.wiki_review_entry(Some(&ws), &entry_id, "undo").expect("workspace undo");
    assert!(rt.space_entries("local-user", &sid).unwrap().iter().all(|e| e.id != entry_id));
}
