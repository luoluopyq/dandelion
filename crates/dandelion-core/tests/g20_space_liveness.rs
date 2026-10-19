//! G20: WITHDRAWN evidence is UNAVAILABLE (work order 4bce §五, validation
//! scenario 3). After a document is revoked or deleted, every page standing
//! on it must stop standing on it IMMEDIATELY — no rebuild, no
//! re-materialize in between: the mirror file prunes on the store write,
//! and pages drop out of listings and searches while a direct fetch says
//! 「来源已撤下」 instead of serving withdrawn text.
//!
//! The rule is keyed on the page's RECORDED evidence (the registry inputs
//! the publish gate wrote — the stable document ids the work order demands
//! pages keep citing), not on prose: bodies are prose a reader may edit,
//! and a citation line deleted by hand must not revive a page standing on
//! withdrawn material. The store arms (lexical, vector) already filter
//! revoked/deleted versions; the disk arms go through this gate. What only
//! the real runtime can prove:
//!
//! | rule | test |
//! |------|------|
//! | revoking a document's live version prunes its mirror file on the spot, withdraws every page whose recorded evidence includes it (listing, direct fetch → 「来源已撤下」, every search surface, `space_ask`) — and re-publishing the document restores every page verbatim (bodies were never destroyed) | `revoke_withdraws_citing_pages_immediately` |
//! | deleting a document does the same INCLUDING the overview, while the LIVE corpus still answers for what remains (the surviving document's raw mirror and lexical arms keep serving) | `delete_withdraws_citing_pages_including_overview` |
//! | the filter is keyed on mirror identity and applied PER PAGE: a space bound to a REAL project serves a page whose input name merely LOOKS like a withdrawn mirror file; the same space filters that page the moment the registration names its own mirror, while a sibling page recorded against a LIVE document stands untouched; a space that was never deep-built is not touched at all — revoke creates no mirror | `withdrawal_filter_is_keyed_on_mirror_identity` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki::{self, AI_OVERVIEW};
use dandelion_core::{KnowledgeRole, PermissionMode, Runtime, Settings};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

/// Scope + space + the two published documents (ids are minted through the
/// real Store API, never names).
struct Ctx {
    rt: Runtime,
    t: String,
    c: String,
    s: String,
    doc_a: String,
    ver_a: String,
    doc_b: String,
}

fn runtime_at(data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.wiki_build_max_articles = 2;
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

const DOC_A: &str = "报价折扣超过五个点须总监签字。\n三个点以内的折扣由销售自行决定。\n所有特价必须当日备案。\n";
const DOC_B: &str = "模具温度波动会导致缩水。\n保压时间不足会出现缩痕。\n周期缩短两成属于正常波动。\n";

fn import_publish(
    rt: &Runtime,
    t: &str,
    c: &str,
    s: &str,
    source_id: &str,
    content: &str,
) -> (String, String) {
    let (doc, version) =
        rt.knowledge_import_document(t, c, s, "boss", "note", source_id, content, 500).unwrap();
    rt.knowledge_publish_version(t, c, s, "boss", &doc.id, &version.id).unwrap();
    (doc.id, version.id)
}

/// Mirror filename for a source suffix (names are deterministic, never
/// guessed — the document id prefixes the file).
fn mirror_named(sources: &std::path::Path, suffix: &str) -> String {
    fs::read_dir(sources)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(suffix))
        .unwrap_or_else(|| panic!("no mirror file ending in {suffix} in {:?}", sources))
}

/// Publish a two-article space Wiki over DOC_A + DOC_B exactly the way the
/// deep build does (materialize → 4 scripted turns), returning the live
/// runtime and the minted ids. NOTE what the build RECORDS: the publish
/// gate snapshots the evidence the pack actually showed, and at this
/// fixture's size every pack shows both mirrors — so every registry page
/// stands on BOTH documents, and withdrawing either one takes all three
/// pages down. The per-page split (some pages stand, some fall) is what
/// `withdrawal_filter_is_keyed_on_mirror_identity` pins with a
/// registry-fixture space.
async fn build_two_article_space(data: &std::path::Path, name: &str) -> Ctx {
    let rt = runtime_at(data, Arc::new(ScriptedProvider::new(Vec::new())));
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let space = rt
        .knowledge_create_space(&tenant.id, &customer.id, name, "allow_summary")
        .unwrap();
    rt.knowledge_set_member(&space.id, "boss", KnowledgeRole::Owner).unwrap();
    rt.knowledge_set_member(&space.id, "reader1", KnowledgeRole::Reader).unwrap();
    let (t, c, s) = (tenant.id, customer.id, space.id);
    let (doc_a, ver_a) = import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);
    let (doc_b, _ver_b) = import_publish(&rt, &t, &c, &s, "policy-b", DOC_B);
    let (sources, _) = rt.space_materialize_sources("boss", &s).unwrap();
    let fa = mirror_named(&sources, "__policy-a.md");
    let fb = mirror_named(&sources, "__policy-b.md");
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::text(format!(
            "```json\n[{{\"title\":\"报价折扣权限\",\"focus\":\"谁能批多少折扣\",\"inputs\":[\"{fa}\"]}},\
             {{\"title\":\"注塑缺陷\",\"focus\":\"缩水与缩痕的成因\",\"inputs\":[\"{fb}\"]}}]\n```"
        )),
        ScriptedTurn::text(format!("五个点以上须总监签字，特价须备案。\n依据 `{fa}:L1-L2`。\n")),
        ScriptedTurn::text(format!("温度波动出缩水，保压不足出缩痕。\n依据 `{fb}:L1-L3`。\n")),
        ScriptedTurn::text(format!(
            "本库覆盖报价与注塑两类资料。依据 `{fb}:L2-L2`。## 学习顺序\n1. [报价折扣权限](TOPIC-1.md)\n2. [注塑缺陷](TOPIC-2.md)"
        )),
    ]));
    let rt2 = runtime_at(data, provider.clone());
    let report = rt2.space_build_llm("boss", &s, false).await.expect("Editor build succeeds");
    assert_eq!(report.published.len(), 3, "fixture: both articles + overview published");
    Ctx { rt: rt2, t, c, s, doc_a, ver_a, doc_b }
}

fn listed(rt: &Runtime, space: &str) -> Vec<String> {
    rt.space_show("reader1", space, None).unwrap().into_iter().map(|p| p.name).collect()
}

fn cites(hit: &dandelion_core::space_qa::SpaceEvidence, token: &str) -> bool {
    hit.label.contains(token) || hit.excerpt.contains(token)
}

/// The withdrawn-page contract, asserted on every built page name.
fn assert_withdrawn(rt: &Runtime, s: &str, page: &str) {
    let stub = rt.space_show("reader1", s, Some(page)).unwrap();
    assert_eq!(stub.len(), 1, "a withdrawn page keeps its IDENTITY");
    assert_eq!(stub[0].name, page);
    assert!(stub[0].body.contains("来源已撤下"), "{page}: {}", stub[0].body);
    assert!(!stub[0].body.contains("总监签字"), "{page}: withdrawn text must not be served");
    assert!(!stub[0].body.contains("缩痕"), "{page}: withdrawn text must not be served");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_withdraws_citing_pages_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ctx = build_two_article_space(&data, "撤销即失效空间").await;
    let rt = &ctx.rt;
    let sources = wiki::space_data_dir(&data, &ctx.s).join("sources");
    let file_a = sources.join(mirror_named(&sources, "__policy-a.md"));
    let t1 = wb::topic_file_for("报价折扣权限");
    let t2 = wb::topic_file_for("注塑缺陷");

    // Before: everything cites DOC_A freely — listing, direct fetch, search.
    let before_list = listed(rt, &ctx.s);
    assert!(before_list.contains(&t1) && before_list.contains(&t2));
    assert!(rt.space_show("reader1", &ctx.s, Some(&t1)).unwrap()[0].body.contains("总监签字"));
    let before = rt.space_search("reader1", &[ctx.s.clone()], "总监签字", 5).unwrap();
    assert!(before.hits.iter().any(|h| cites(h, "总监签字")), "{:?}", before.hits);

    // Revoke the document's live version. NOTHING else runs — no build, no
    // materialize. The mirror file is gone ON THE SPOT…
    rt.knowledge_revoke_version(&ctx.t, &ctx.c, &ctx.s, "boss", &ctx.doc_a, &ctx.ver_a).unwrap();
    assert!(!file_a.exists(), "the mirror prunes with the store write, not at the next build");

    // …and every page whose recorded evidence includes DOC_A is withdrawn
    // immediately — here that is all three, because the publish gate
    // records the evidence the pack actually showed, and at this size
    // every pack showed both mirrors. The rule is fail-closed on the
    // AUDITED evidence set, not on what the prose happens to quote.
    let now = listed(rt, &ctx.s);
    for page in [t1.as_str(), t2.as_str(), AI_OVERVIEW] {
        assert!(!now.iter().any(|p| p == page), "page citing withdrawn material is gone: {page} in {now:?}");
    }

    // …a direct fetch keeps the page's IDENTITY but never its content…
    assert_withdrawn(rt, &ctx.s, &t1);
    assert_withdrawn(rt, &ctx.s, &t2);
    assert_withdrawn(rt, &ctx.s, AI_OVERVIEW);

    // …and NO search surface cites it: the article, raw-mirror and lexical
    // arms all lose the withdrawn evidence (the store arms already filter
    // revoked versions; the disk arms go through the liveness gate).
    let after = rt.space_search("reader1", &[ctx.s.clone()], "总监签字", 5).unwrap();
    assert!(
        after.hits.iter().all(|h| !cites(h, "总监签字") && !cites(h, &t1)),
        "withdrawn evidence must not surface: {:?}",
        after.hits
    );
    let ask = rt.space_ask("reader1", &[ctx.s.clone()], "总监签字", 5, false).await.unwrap();
    assert!(ask.no_answer, "the QA turn may not answer from withdrawn prose");

    // Re-publishing the document restores every page VERBATIM — withdrawal
    // hid the bodies, it did not destroy them. The import rides the same
    // source id, so the stable document id comes back live.
    let (doc_a2, ver_a2) = import_publish(rt, &ctx.t, &ctx.c, &ctx.s, "policy-a", DOC_A);
    assert_eq!(doc_a2, ctx.doc_a, "the source id keeps its stable document id");
    let _ = ver_a2;
    let list = listed(rt, &ctx.s);
    assert!(list.contains(&t1) && list.contains(&t2) && list.iter().any(|p| p == AI_OVERVIEW), "re-publishing restores the listing: {list:?}");
    let body = rt.space_show("reader1", &ctx.s, Some(&t1)).unwrap();
    assert!(body[0].body.contains("总监签字"), "the original body comes back whole");
    let back = rt.space_search("reader1", &[ctx.s.clone()], "总监签字", 5).unwrap();
    assert!(back.hits.iter().any(|h| cites(h, "总监签字")), "{:?}", back.hits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_withdraws_citing_pages_including_overview() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ctx = build_two_article_space(&data, "删除即失效空间").await;
    let rt = &ctx.rt;
    let sources = wiki::space_data_dir(&data, &ctx.s).join("sources");
    let file_b = sources.join(mirror_named(&sources, "__policy-b.md"));
    let t1 = wb::topic_file_for("报价折扣权限");
    let t2 = wb::topic_file_for("注塑缺陷");

    // Deleting DOC_B withdraws EVERY page whose recorded evidence includes
    // it — the article built from it, the overview that cited one line of
    // it, AND the sibling article whose prose never quotes it (its pack
    // showed DOC_B, and the audited evidence set is the contract). The
    // rule does not rank pages.
    rt.knowledge_delete_document(&ctx.t, &ctx.c, &ctx.s, "boss", &ctx.doc_b).unwrap();
    assert!(!file_b.exists(), "the deleted document's mirror file prunes on the spot");
    let now = listed(rt, &ctx.s);
    for page in [t1.as_str(), t2.as_str(), AI_OVERVIEW] {
        assert!(!now.iter().any(|p| p == page), "page standing on deleted material is gone: {page} in {now:?}");
    }
    assert_withdrawn(rt, &ctx.s, &t2);
    assert_withdrawn(rt, &ctx.s, AI_OVERVIEW);

    // NOTHING withdrawn survives in any search surface or the QA turn.
    let after = rt.space_search("reader1", &[ctx.s.clone()], "缩痕", 5).unwrap();
    assert!(
        after.hits.iter().all(|h| !cites(h, "缩痕")),
        "withdrawn evidence must not surface: {:?}",
        after.hits
    );
    let ask = rt.space_ask("reader1", &[ctx.s.clone()], "缩痕", 5, false).await.unwrap();
    assert!(ask.no_answer, "the QA turn answers honestly instead of from withdrawn prose");

    // And the LIVE document still answers for itself: DOC_A was not
    // deleted, so its raw mirror and store arms keep serving — withdrawal
    // is per document, not a space-wide blackout of everything built.
    let ok = rt.space_search("reader1", &[ctx.s.clone()], "总监签字", 5).unwrap();
    assert!(ok.hits.iter().any(|h| cites(h, "总监签字")), "live corpus still answers: {:?}", ok.hits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn withdrawal_filter_is_keyed_on_mirror_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let space = rt
        .knowledge_create_space(&tenant.id, &customer.id, "真实项目空间", "allow_summary")
        .unwrap();
    let s = space.id;
    rt.knowledge_set_member(&s, "boss", KnowledgeRole::Owner).unwrap();
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();

    // A live document gives the space a real mirror file; a quick-scan Wiki
    // carries two hand-written pages — one standing on that LIVE document,
    // one whose recorded input name merely LOOKS like the mirror file of a
    // document that was never published.
    let (doc_c, _ver_c) = import_publish(&rt, &tenant.id, &customer.id, &s, "policy-c", "丙供设备须每月点检并留痕。\n");
    let (sources, _) = rt.space_materialize_sources("boss", &s).unwrap();
    let fc = mirror_named(&sources, "__policy-c.md");
    let proj = tmp.path().join("proj");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join("notes.md"), "甲供材进场须提前四十八小时通知。\n").unwrap();
    let store = dandelion_core::store::sqlite::Store::open(&data, 5000).unwrap();
    store.knowledge_register_workspace(&s, "ws-real", &proj.to_string_lossy()).unwrap();
    let sdir = wiki::space_data_dir(&data, &s);
    fs::create_dir_all(wiki::compiled_dir(&sdir)).unwrap();
    fs::write(wiki::compiled_dir(&sdir).join("甲供材.md"), "进场须提前四十八小时通知监理。\n").unwrap();
    fs::write(wiki::compiled_dir(&sdir).join("丙供设备.md"), "丙供设备每月点检并留痕备查。\n").unwrap();
    let mut dead_inputs = BTreeMap::new();
    dead_inputs.insert("dead00000000__notes.md".to_string(), "sha256:nonexistent".to_string());
    let mut live_inputs = BTreeMap::new();
    live_inputs.insert(fc.clone(), "sha256:whatever".to_string());
    let article = |title: &str, inputs: BTreeMap<String, String>| wb::ArticleRecord {
        title: title.into(),
        focus: String::new(),
        build: 1,
        inputs,
        published_at: "2026-10-16T00:00:00Z".into(),
        user_edited: false,
        ranges: Vec::new(),
    };
    let reg = wb::ArticleRegistry {
        version: 1,
        articles: BTreeMap::from([
            ("甲供材.md".to_string(), article("甲供材", dead_inputs)),
            ("丙供设备.md".to_string(), article("丙供设备", live_inputs)),
        ]),
    };
    wb::save_articles(&sdir, &reg).unwrap();
    let _ = doc_c;

    // Bound to a REAL project the space is never filtered — names are not
    // identity, and the withdrawal gate is keyed on the registered
    // workspace being the space's own mirror.
    let pages = rt.space_show("reader1", &s, None).unwrap();
    assert!(pages.iter().any(|p| p.name == "甲供材.md"), "real-project pages are never filtered");
    let one = rt.space_show("reader1", &s, Some("甲供材")).unwrap();
    assert!(one[0].body.contains("四十八小时"), "{}", one[0].body);
    let hits = rt.space_search("reader1", &[s.clone()], "四十八小时", 5).unwrap();
    assert!(hits.hits.iter().any(|h| cites(h, "四十八小时")), "{:?}", hits.hits);

    // Flip the registration to the space's OWN mirror path (what a deep
    // build registers). The same read now goes through the liveness gate —
    // and it discriminates PER PAGE off the recorded evidence, not per
    // space: the LOOKALIKE page falls to 「来源已撤下」 while the page
    // standing on the LIVE document is untouched. No rebuild in between.
    store
        .knowledge_register_workspace(&s, "ws-mirror", &sources.to_string_lossy())
        .unwrap();
    let stub = rt.space_show("reader1", &s, Some("甲供材")).unwrap();
    assert!(stub[0].body.contains("来源已撤下"), "{}", stub[0].body);
    let now = listed(&rt, &s);
    assert!(!now.contains(&"甲供材.md".to_string()), "{now:?}");
    assert!(now.contains(&"丙供设备.md".to_string()), "the live-evidence page stands: {now:?}");
    let live = rt.space_show("reader1", &s, Some("丙供设备")).unwrap();
    assert!(live[0].body.contains("点检"), "served whole, no stub: {}", live[0].body);
    let hits = rt.space_search("reader1", &[s.clone()], "四十八小时", 5).unwrap();
    assert!(hits.hits.iter().all(|h| !cites(h, "四十八小时")), "{:?}", hits.hits);
    let hits = rt.space_search("reader1", &[s.clone()], "点检", 5).unwrap();
    assert!(hits.hits.iter().any(|h| cites(h, "点检")), "live page keeps answering: {:?}", hits.hits);

    // A space that was never deep-built has no registered mirror: revoke
    // there must NOT so much as create the mirror directory — the resync
    // leaves untouched spaces untouched.
    let raw = rt
        .knowledge_create_space(&tenant.id, &customer.id, "未构建空间", "allow_summary")
        .unwrap();
    rt.knowledge_set_member(&raw.id, "boss", KnowledgeRole::Owner).unwrap();
    let (doc_r, ver_r) = import_publish(&rt, &tenant.id, &customer.id, &raw.id, "policy-r", "押金须在验收后七日内退还。\n");
    rt.knowledge_revoke_version(&tenant.id, &customer.id, &raw.id, "boss", &doc_r, &ver_r).unwrap();
    assert!(
        !wiki::space_data_dir(&data, &raw.id).join("sources").exists(),
        "the immediate resync never creates a mirror for an unbuilt space"
    );
}
