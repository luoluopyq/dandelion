//! G24: A CITATION CLICK OPENS THE VERSION IT WAS WRITTEN AGAINST (work
//! order 4bce §八, §十一-10). Every published page line `路径:Lx-Ly` is a
//! click target; clicking must answer with the FROZEN source version the
//! build actually read — never a later version, never a silently-empty
//! panel, and never an answer that dresses a legal locator up as a proof.
//!
//! The recording side runs INSIDE the space build (`space_build_llm` calls
//! `space_record_evidence_spans` after publishing): every citation that
//! survived the publish gate on a published page — machine or hand-written
//! — is frozen against the live version that build read. The read side
//! (`space_evidence`) rides the same fail-closed gate family as every other
//! space read, and every refusal is named:
//!
//! | rule | test |
//! |------|------|
//! | a cited token opened by a READER resolves to the exact version, lines and full content the build read, carries the parser version, and ships the 「位置合法不等于结论已证实」 notice verbatim | `click_resolves_to_the_frozen_version` |
//! | refusals are named, never empty: an unparsable token, a token no published page cites, and a non-member all fail closed with their own reason | `unrecorded_and_illegal_clicks_are_named` |
//! | revoked and deleted sources answer 「来源已撤下」 in their own words; a SUPERSEDED version still resolves with its frozen text (supersede is not revoke, A5) | `withdrawn_clicks_refuse_superseded_clicks_answer` |
//! | the recorder reads pages exactly as the reader reads them — hand-written pages are recorded at the next re-record, non-mirror rels are skipped honestly, and a space that was never built records nothing | `recorder_follows_the_reader_and_skips_honestly` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::{KnowledgeRole, PermissionMode, Runtime, Settings};
use std::fs;
use std::sync::Arc;

fn runtime_at(data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.wiki_build_max_articles = 2;
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

const DOC_A: &str = "报价折扣超过五个点须总监签字。\n三个点以内的折扣由销售自行决定。\n所有特价必须当日备案。\n";
const DOC_B: &str = "模具温度波动会导致缩水。\n保压时间不足会出现缩痕。\n周期缩短两成属于正常波动。\n";

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

struct Fixture {
    rt: Runtime,
    t: String,
    c: String,
    s: String,
    doc_a: String,
    ver_a: String,
    doc_b: String,
    fa: String,
    fb: String,
}

/// The g20 build fixture verbatim in shape: scope → two published docs →
/// materialize → 4 scripted turns → deep build publishes 3 pages. The deep
/// build now ALSO records the evidence spans, which is what every test
/// below clicks.
async fn build(data: &std::path::Path, name: &str) -> Fixture {
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
    Fixture { rt: rt2, t, c, s, doc_a, ver_a, doc_b, fa, fb }
}

fn import_publish(rt: &Runtime, t: &str, c: &str, s: &str, source_id: &str, content: &str) -> (String, String) {
    let (doc, version) =
        rt.knowledge_import_document(t, c, s, "boss", "note", source_id, content, 500).unwrap();
    rt.knowledge_publish_version(t, c, s, "boss", &doc.id, &version.id).unwrap();
    (doc.id, version.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn click_resolves_to_the_frozen_version() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let fx = build(&data, "出处可打开空间").await;
    let rt = &fx.rt;

    // A READER clicks the `fa:L1-L2` token the article body carries. The
    // answer is the version the build read, line-for-line — plus the full
    // stored content, the parser version (§三), and the §八 notice.
    let v = rt.space_evidence("reader1", &fx.s, &format!("{}:L1-L2", fx.fa)).expect("recorded click resolves");
    assert_eq!(v["documentId"], fx.doc_a.as_str(), "stable document id, not a name");
    assert_eq!(v["versionNo"], 1, "the build read version 1 — click resolves to THAT version");
    assert_eq!(v["parserVersion"], 1, "§三: the source version carries its parser version");
    assert_eq!(v["sourceKind"], "note");
    assert_eq!(v["sourceId"], "policy-a");
    assert_eq!(v["homeSpaceId"], fx.s.as_str());
    assert_eq!(v["crossSpace"], false, "a home-space material is not a cross-space click");
    assert_eq!(v["superseded"], false);
    assert_eq!(v["locator"], format!("{}#L1-L2", fx.fa).as_str(), "the display locator is the Line form");
    assert_eq!(v["totalLines"], 3);
    assert_eq!(v["rangeValid"], true);
    let excerpt = v["excerpt"].as_array().unwrap();
    assert_eq!(excerpt.len(), 2, "exactly the cited lines");
    assert_eq!(excerpt[0]["line"], 1);
    assert_eq!(excerpt[0]["text"], "报价折扣超过五个点须总监签字。");
    assert_eq!(excerpt[1]["line"], 2);
    assert_eq!(excerpt[1]["text"], "三个点以内的折扣由销售自行决定。");
    assert_eq!(v["content"], DOC_A, "the full FROZEN content rides along (原文/快照)");
    let notice = v["notice"].as_str().unwrap();
    assert!(notice.contains("v1") && notice.contains("不等于结论已证实"), "§八: {notice}");

    // The overview cited `{fb}:L2-L2`; the UI sends the single-line form
    // for such a click and it must hit the same canonical token.
    let single = rt.space_evidence("reader1", &fx.s, &format!("{}:L2", fx.fb)).expect("single-line click canonicalizes");
    assert_eq!(single["documentId"], fx.doc_b.as_str());
    assert_eq!(single["excerpt"].as_array().unwrap().len(), 1);
    assert_eq!(single["excerpt"][0]["text"], "保压时间不足会出现缩痕。");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrecorded_and_illegal_clicks_are_named() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let fx = build(&data, "拒绝具名空间").await;
    let rt = &fx.rt;

    // Not a citation at all → its own named refusal (never an empty panel).
    let bad = rt.space_evidence("reader1", &fx.s, "随便一句话").unwrap_err().to_string();
    assert!(bad.contains("引用格式无法解析"), "{bad}");

    // A well-formed token NO published page carries → 「无法定位引用来源」,
    // with the remedy named (rebuild to record new hand-written citations).
    let ghost = rt.space_evidence("reader1", &fx.s, &format!("{}:L99-L99", fx.fa)).unwrap_err().to_string();
    assert!(ghost.contains("无法定位引用来源"), "{ghost}");

    // The read gate is the same fail-closed family as every other space
    // read: a non-member gets nothing, whatever the token.
    assert!(rt.space_evidence("nobody", &fx.s, &format!("{}:L1-L2", fx.fa)).is_err());
    // A space that does not exist is refused by identity, not by name.
    assert!(rt.space_evidence("reader1", "sp-nonexistent", "a.md:L1").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn withdrawn_clicks_refuse_superseded_clicks_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let fx = build(&data, "撤下与接替空间").await;
    let rt = &fx.rt;

    // Revoking the source version withdraws the CLICK too — 「来源已撤下」,
    // same wording family as the withdrawn pages (A3), version named.
    rt.knowledge_revoke_version(&fx.t, &fx.c, &fx.s, "boss", &fx.doc_a, &fx.ver_a).unwrap();
    let revoked = rt.space_evidence("reader1", &fx.s, &format!("{}:L1-L2", fx.fa)).unwrap_err().to_string();
    assert!(revoked.contains("来源已撤下") && revoked.contains("已被撤销"), "{revoked}");

    // SUPERSEDE IS NOT REVOKE (A5), shown on doc_b — doc_a's v1 is already
    // revoked above and revocation is permanent, so it cannot double as a
    // clean supersede subject. A new version published over v1 supersedes
    // it, but a click recorded against v1 still resolves to v1's FROZEN
    // text — the newer wording must NOT leak into it — and the payload
    // says so (superseded: true). Re-import the same source id so the
    // stable document id keeps its single version history.
    let doc_b2 = {
        let (doc, version) = rt
            .knowledge_import_document(&fx.t, &fx.c, &fx.s, "boss", "note", "policy-b", &format!("{DOC_B}缩痕整改须附评审单号。\n"), 500)
            .unwrap();
        rt.knowledge_publish_version(&fx.t, &fx.c, &fx.s, "boss", &doc.id, &version.id).unwrap();
        assert_eq!(doc.id, fx.doc_b, "same source id, same stable document, one history");
        doc.id
    };
    let _ = doc_b2;
    let old = rt
        .space_evidence("reader1", &fx.s, &format!("{}:L1-L3", fx.fb))
        .expect("a superseded-but-not-revoked version still answers");
    assert_eq!(old["versionNo"], 1, "the click stays on the version it was recorded against");
    assert_eq!(old["superseded"], true, "and says it has been superseded");
    assert_eq!(old["content"], DOC_B, "frozen text verbatim — no newer wording leaks in");
    assert!(!old["content"].as_str().unwrap().contains("评审单号"));
    // And no click appears for a line NO published page cited: the frozen
    // token set is the contract, not "any line of the current version".
    let fresh = rt.space_evidence("reader1", &fx.s, &format!("{}:L4", fx.fb)).unwrap_err().to_string();
    assert!(fresh.contains("无法定位引用来源"), "uncited lines are not phantom clicks: {fresh}");

    // Deleting a document (v1 AND its superseding v2): the version rows
    // cascade, so the honest answer is that the material itself is gone —
    // still 「来源已撤下」 family.
    rt.knowledge_delete_document(&fx.t, &fx.c, &fx.s, "boss", &fx.doc_b).unwrap();
    let gone = rt.space_evidence("reader1", &fx.s, &format!("{}:L1-L3", fx.fb)).unwrap_err().to_string();
    assert!(gone.contains("来源已撤下") && gone.contains("已被删除"), "{gone}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recorder_follows_the_reader_and_skips_honestly() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let space = rt
        .knowledge_create_space(&tenant.id, &customer.id, "人写引用空间", "allow_summary")
        .unwrap();
    let s = space.id;
    rt.knowledge_set_member(&s, "boss", KnowledgeRole::Owner).unwrap();
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();

    // Never-built: nothing to read means nothing to record, honestly zero.
    assert_eq!(rt.space_record_evidence_spans("boss", &s).unwrap(), 0);

    let (doc_a, ver_a) = import_publish(&rt, &tenant.id, &customer.id, &s, "policy-a", DOC_A);
    let (sources, _) = rt.space_materialize_sources("boss", &s).unwrap();
    let fa = mirror_named(&sources, "__policy-a.md");
    // A hand-written page (§A4) citing a REAL line of the live document and
    // a LOOKALIKE of a document that was never published.
    rt.space_page_save(
        "boss",
        &s,
        "手工页",
        &format!("签字规则见 `{fa}:L3`。另有旧资料 `dead00000000__notes.md:L1`。\n"),
    )
    .unwrap();
    // Re-record sees pages EXACTLY as the reader sees them: the hand page
    // is published (switch_version) and its live-document citation lands;
    // the lookalike rel has no live document behind it and is skipped.
    let recorded = rt.space_record_evidence_spans("boss", &s).unwrap();
    assert_eq!(recorded, 1, "only the citation with a live document behind it: {recorded}");
    let v = rt.space_evidence("reader1", &s, &format!("{fa}:L3")).expect("hand-written citation is a real click target");
    assert_eq!(v["documentId"], doc_a.as_str());
    assert_eq!(v["excerpt"][0]["text"], "所有特价必须当日备案。");
    let skip = rt.space_evidence("reader1", &s, "dead00000000__notes.md:L1").unwrap_err().to_string();
    assert!(skip.contains("无法定位引用来源"), "non-mirror rels are skipped, not phantom-frozen: {skip}");

    // Re-recording REPLACES the set atomically — withdraw the document's
    // live version, re-record, and the token that was there a moment ago
    // no longer has a live document to freeze against…
    let pages = rt.space_show("reader1", &s, None).unwrap();
    assert!(pages.iter().any(|p| p.name == "手工页.md"));
    // ver_a is still the live published version — nothing new was published
    // here, so the import-time handle names it exactly (no store-level peek).
    rt.knowledge_revoke_version(&tenant.id, &customer.id, &s, "boss", &doc_a, &ver_a).unwrap();
    // …but the CLICK already recorded still answers, because it points at
    // a REVOKEABLE version row, and revocation changes the answer from
    // content to 「来源已撤下」 — not from content to silence-by-absence.
    let after = rt.space_evidence("reader1", &s, &format!("{fa}:L3")).unwrap_err().to_string();
    assert!(after.contains("来源已撤下"), "{after}");
    // A hand page KEEPS standing in the listing. This is A3's deliberate
    // scope, not a hole: withdrawal reads the AUDITED BUILD REGISTRY,
    // never the prose (an owner hand-deleting a citation line must not
    // revive a dead machine page — the inverse reading would equally let
    // guessed prose text bury a live one). A page the registry never
    // audited has no evidence to withdraw; its owner wrote it, and its
    // frozen click above already says 「来源已撤下」 (g20 pins the machine
    // -page withdrawal this scope excludes nothing of).
    let now = rt.space_show("reader1", &s, None).unwrap();
    assert!(now.iter().any(|p| p.name == "手工页.md"), "hand pages are not silently eaten: {:?}", now.iter().map(|p| &p.name).collect::<Vec<_>>());
    // And a re-record after the withdrawal freezes NO phantom version.
    assert_eq!(rt.space_record_evidence_spans("boss", &s).unwrap(), 0);
    let gone = rt.space_evidence("reader1", &s, &format!("{fa}:L3")).unwrap_err().to_string();
    assert!(gone.contains("无法定位引用来源"), "the phantom is gone with the re-record: {gone}");
    let _ = wb::topic_file_for("never-used"); // keep the wiki_build import honest in cfgs
}
