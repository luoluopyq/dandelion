//! G22: §三 空间关联 — one material, many spaces, one history.
//!
//! A material is identified by (tenant, customer, source kind, source id),
//! NOT by where it was first imported. Re-importing the same source into
//! another space must find the SAME document row with the SAME version
//! history; which spaces see it and WHICH version each sees lives only in
//! the link table (`follow` / `pinned`). Publishing supersedes; only
//! revoking withdraws — that difference is what makes a fixed historical
//! version both real AND killable.
//!
//! | rule | test |
//! |------|------|
//! | the same source re-imported through a second space is the SAME document with ONE shared history; both spaces list it, home stays home | `same_source_through_two_spaces_is_one_document_one_history` |
//! | `follow` advances on publish; `pinned` freezes this space on its version (superseded is still citable) while the other space moves on; unpinning catches up — and every read surface (material list, search, mirror bytes) agrees | `follow_advances_pinned_freezes_and_unpin_catches_up` |
//! | pinning refuses drafts, unknown binding words, missing numbers and unlinked materials; but WITHDRAWING a pinned version kills it here while the material's live version keeps answering elsewhere | `pin_refuses_fakes_and_withdrawal_kills_a_pin` |
//! | linking is membership, never a read-through: Editor here, Reader at home, same tenant+customer, and ghosts/roles refuse before anything moves | `link_gates_never_read_through_a_wall` |
//! | unlink prunes THIS space only — the material stays live for its home, a non-home delete is refused, and relinking is possible | `unlink_is_local_delete_is_not_and_relink_works` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::ScriptedProvider;
use dandelion_core::{KnowledgeRole, PermissionMode, Runtime, Settings};
use std::fs;
use std::sync::Arc;

fn runtime_at(data: &std::path::Path) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(data.to_path_buf())),
        settings,
        Arc::new(ScriptedProvider::new(Vec::new())),
    )
    .expect("runtime")
}

fn two_spaces(rt: &Runtime) -> (String, String, String, String) {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let s1 = rt
        .knowledge_create_space(&tenant.id, &customer.id, "报价规则空间", "allow_summary")
        .unwrap();
    let s2 = rt
        .knowledge_create_space(&tenant.id, &customer.id, "生产班组空间", "allow_summary")
        .unwrap();
    rt.knowledge_set_member(&s1.id, "boss", KnowledgeRole::Owner).unwrap();
    rt.knowledge_set_member(&s2.id, "boss", KnowledgeRole::Owner).unwrap();
    (tenant.id, customer.id, s1.id, s2.id)
}

// Two revisions of ONE source. The phrases are version-unique so retrieval,
// the material list and the mirror bytes can all be asked WHICH version
// this space is actually reading.
const V1: &str = "报价折扣超过五个点须总监签字。\n特价必须当日备案。\n";
const V2: &str = "报价折扣超过八个点须副总裁签字。\n特价必须三日备案。\n";

fn import(rt: &Runtime, t: &str, c: &str, s: &str, source_id: &str, content: &str) -> (String, i64, String) {
    let (doc, version) = rt
        .knowledge_import_document(t, c, s, "boss", "note", source_id, content, 500)
        .unwrap();
    (doc.id, version.version_no, version.id)
}

fn publish(rt: &Runtime, t: &str, c: &str, home: &str, doc_id: &str, version_id: &str) {
    rt.knowledge_publish_version(t, c, home, "boss", doc_id, version_id)
        .unwrap();
}

/// The mirror file for a source id (names are deterministic: the document
/// id prefixes `<doc>__<source>.md`, so the suffix finds it exactly).
fn mirror_file(sources: &std::path::Path, suffix: &str) -> String {
    let name = fs::read_dir(sources)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(suffix))
        .unwrap_or_else(|| panic!("no mirror file ending in {suffix} in {:?}", sources));
    fs::read_to_string(sources.join(name)).unwrap()
}

fn resolved(rt: &Runtime, space: &str, doc_id: &str) -> Option<i64> {
    rt.space_materials("boss", space)
        .unwrap()
        .into_iter()
        .find(|m| m.document_id == doc_id)
        .unwrap_or_else(|| panic!("material {doc_id} not listed in {space}"))
        .resolved_version_no
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_source_through_two_spaces_is_one_document_one_history() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime_at(&tmp.path().join("data"));
    let (t, c, s1, s2) = two_spaces(&rt);

    let (doc1, no1, v1) = import(&rt, &t, &c, &s1, "quote-policy", V1);
    publish(&rt, &t, &c, &s1, &doc1, &v1);
    assert_eq!(no1, 1);

    // Same source through the OTHER space: not a second document — the
    // material identity is (tenant, customer, kind, source id), so this
    // continues the one existing history as version 2 of doc1.
    let (doc2, no2, v2) = import(&rt, &t, &c, &s2, "quote-policy", V2);
    assert_eq!(doc2, doc1, "re-import must find the SAME material");
    assert_eq!(no2, 2, "and continue the ONE version history");

    // Home stays home: the document row was never re-parented.
    assert_eq!(rt.space_materials("boss", &s1).unwrap()[0].home_space_id, s1);
    assert_eq!(rt.space_materials("boss", &s2).unwrap()[0].home_space_id, s1);

    // ONE history means every space's portable export carries the SAME
    // chain — both revisions, once each, not per-space forks.
    for space in [&s1, &s2] {
        let export = rt.knowledge_export(&t, &c, space, "boss").unwrap();
        let ours: Vec<_> = export["versions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["documentId"] == serde_json::json!(doc1))
            .collect();
        let nos: Vec<i64> = ours.iter().map(|v| v["versionNo"].as_i64().unwrap()).collect();
        assert_eq!(nos, vec![1, 2], "space {space} must see the single shared history");
    }

    // Publishing v2 via the home space advances BOTH following spaces.
    publish(&rt, &t, &c, &s1, &doc1, &v2);
    assert_eq!(resolved(&rt, &s1, &doc1), Some(2));
    assert_eq!(resolved(&rt, &s2, &doc1), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_advances_pinned_freezes_and_unpin_catches_up() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime_at(&tmp.path().join("data"));
    let (t, c, s1, s2) = two_spaces(&rt);

    let (doc, _, v1) = import(&rt, &t, &c, &s1, "quote-policy", V1);
    publish(&rt, &t, &c, &s1, &doc, &v1);
    rt.space_link_add("boss", &s2, &doc).unwrap();
    assert_eq!(resolved(&rt, &s2, &doc), Some(1));

    // Pin the PRODUCTION space on v1 before the rule changes.
    rt.space_link_set("boss", &s2, &doc, "pinned", Some(1)).unwrap();

    // The material updates where it lives: v2 supersedes v1.
    let (_, no2, v2) = import(&rt, &t, &c, &s1, "quote-policy", V2);
    assert_eq!(no2, 2);
    publish(&rt, &t, &c, &s1, &doc, &v2);

    // The frozen space keeps reading v1 — superseded is not withdrawn;
    // that difference IS what a fixed historical version means.
    assert_eq!(resolved(&rt, &s1, &doc), Some(2), "the home space followed");
    assert_eq!(resolved(&rt, &s2, &doc), Some(1), "the pinned space did not");

    // Retrieval agrees: the pinned space can never answer from v2 text.
    let hits = rt.knowledge_search(&t, &c, &s2, "boss", "三日备案", 5).unwrap();
    assert!(hits.is_empty(), "pinned space must not answer from v2: {hits:?}");
    let hits = rt.knowledge_search(&t, &c, &s2, "boss", "当日备案", 5).unwrap();
    assert!(!hits.is_empty(), "pinned space must still answer from v1");
    let hits = rt.knowledge_search(&t, &c, &s1, "boss", "三日备案", 5).unwrap();
    assert!(!hits.is_empty(), "following space must answer from v2");

    // The evidence mirror materializes the RESOLVED bytes, per space.
    let (sources2, n2) = rt.space_materialize_sources("boss", &s2).unwrap();
    assert_eq!(n2, 1);
    let body2 = mirror_file(&sources2, "__quote-policy.md");
    assert!(body2.contains("当日备案") && !body2.contains("三日备案"), "pinned mirror: {body2}");
    let (sources1, _) = rt.space_materialize_sources("boss", &s1).unwrap();
    let body1 = mirror_file(&sources1, "__quote-policy.md");
    assert!(body1.contains("三日备案"), "follow mirror: {body1}");

    // Unpin → the frozen space catches the current version up at once.
    rt.space_link_set("boss", &s2, &doc, "follow", None).unwrap();
    assert_eq!(resolved(&rt, &s2, &doc), Some(2));
    let hits = rt.knowledge_search(&t, &c, &s2, "boss", "三日备案", 5).unwrap();
    assert!(!hits.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pin_refuses_fakes_and_withdrawal_kills_a_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime_at(&tmp.path().join("data"));
    let (t, c, s1, s2) = two_spaces(&rt);

    let (doc, _, v1) = import(&rt, &t, &c, &s1, "quote-policy", V1);
    publish(&rt, &t, &c, &s1, &doc, &v1);
    let (_, no2, v2) = import(&rt, &t, &c, &s1, "quote-policy", V2); // draft
    rt.space_link_add("boss", &s2, &doc).unwrap();

    // A draft is not a fact: pinning the unpublished v2 refuses BY NAME.
    let err = rt.space_link_set("boss", &s2, &doc, "pinned", Some(no2)).unwrap_err();
    assert!(err.to_string().contains("pinned version is not a published version"), "{err}");
    let err = rt.space_link_set("boss", &s2, &doc, "pinned", Some(99)).unwrap_err();
    assert!(err.to_string().contains("pinned version is not a published version"), "{err}");
    let err = rt.space_link_set("boss", &s2, &doc, "frozen", Some(1)).unwrap_err();
    assert!(err.to_string().contains("invalid binding"), "{err}");
    let err = rt.space_link_set("boss", &s2, &doc, "pinned", None).unwrap_err();
    assert!(err.to_string().contains("requires a version number"), "{err}");
    let err = rt
        .space_link_set("boss", &s2, "doc_never_linked", "pinned", Some(1))
        .unwrap_err();
    assert!(err.to_string().contains("material is not linked to this space"), "{err}");

    // Freeze on v1, then let the material move on — the pin HOLDS.
    rt.space_link_set("boss", &s2, &doc, "pinned", Some(1)).unwrap();
    publish(&rt, &t, &c, &s1, &doc, &v2);
    assert_eq!(resolved(&rt, &s2, &doc), Some(1));

    // Now WITHDRAW v1 (the historical version itself, via its home space).
    // Fail closed: the pinned space loses this material entirely — while
    // the live v2 keeps serving the following space, and the material's
    // overall status stays published because something IS effective.
    rt.knowledge_revoke_version(&t, &c, &s1, "boss", &doc, &v1).unwrap();
    assert_eq!(resolved(&rt, &s2, &doc), None, "a withdrawn pin resolves to nothing");
    assert_eq!(resolved(&rt, &s1, &doc), Some(2), "the live version is untouched");
    assert!(rt.knowledge_search(&t, &c, &s2, "boss", "当日备案", 5).unwrap().is_empty());
    let mats = rt.space_materials("boss", &s2).unwrap();
    let pinned = mats.iter().find(|m| m.document_id == doc).unwrap();
    assert_eq!(pinned.binding, "pinned");
    assert_eq!(pinned.pinned_version_no, Some(1));
    assert_eq!(pinned.status, "published", "one withdrawn historical version must not demote the material");

    // The pinned space's mirror — if it was ever built — loses the file NOW.
    let (sources2, _) = rt.space_materialize_sources("boss", &s2).unwrap();
    drop(rt.space_materialize_sources("boss", &s2).unwrap());
    let names: Vec<String> = fs::read_dir(&sources2)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|n| n.ends_with("__quote-policy.md")),
        "withdrawal must prune the citing mirror: {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_gates_never_read_through_a_wall() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime_at(&tmp.path().join("data"));
    let (t, c, s1, s2) = two_spaces(&rt);
    rt.knowledge_set_member(&s2, "editor2", KnowledgeRole::Editor).unwrap();
    rt.knowledge_set_member(&s2, "reader2", KnowledgeRole::Reader).unwrap();

    let (doc, _, v1) = import(&rt, &t, &c, &s1, "quote-policy", V1);
    publish(&rt, &t, &c, &s1, &doc, &v1);

    // Editor HERE but nobody at the material's home: linking is membership,
    // never a way to read through a wall.
    let err = rt.space_link_add("editor2", &s2, &doc).unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");
    // Reader here may look, never link.
    let err = rt.space_link_add("reader2", &s2, &doc).unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    let err = rt.space_link_add("ghost", &s2, &doc).unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");
    let err = rt.space_link_add("boss", "space_ghost000", &doc).unwrap_err();
    assert!(err.to_string().contains("知识空间不存在"), "{err}");

    // Same boss, different CUSTOMER: the material's tenant/customer is a
    // wall the link table refuses to bridge, membership or not.
    let customer2 = rt.knowledge_create_customer(&t, "外贸客户乙").unwrap();
    let s3 = rt
        .knowledge_create_space(&t, &customer2.id, "隔壁客户空间", "allow_summary")
        .unwrap();
    rt.knowledge_set_member(&s3.id, "boss", KnowledgeRole::Owner).unwrap();
    let err = rt.space_link_add("boss", &s3.id, &doc).unwrap_err();
    assert!(err.to_string().contains("outside the requested scope"), "{err}");

    // Reading the binding view is a Reader privilege, and the honest
    // unlink of something never linked says so instead of pretending.
    rt.space_link_add("boss", &s2, &doc).unwrap();
    let listed = rt.space_materials("reader2", &s2).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].document_id, doc);
    let err = rt.space_link_set("reader2", &s2, &doc, "follow", None).unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    rt.space_link_remove("boss", &s2, &doc).unwrap();
    let err = rt.space_link_remove("boss", &s2, &doc).unwrap_err();
    assert!(err.to_string().contains("该资料未关联到此知识空间"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlink_is_local_delete_is_not_and_relink_works() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime_at(&tmp.path().join("data"));
    let (t, c, s1, s2) = two_spaces(&rt);

    let (doc, _, v1) = import(&rt, &t, &c, &s1, "quote-policy", V1);
    publish(&rt, &t, &c, &s1, &doc, &v1);
    rt.space_link_add("boss", &s2, &doc).unwrap();
    rt.space_materialize_sources("boss", &s2).unwrap();

    // Unlink prunes ONLY this space's membership (and its mirror).
    rt.space_link_remove("boss", &s2, &doc).unwrap();
    assert!(rt.space_materials("boss", &s2).unwrap().is_empty());
    assert_eq!(resolved(&rt, &s1, &doc), Some(1), "the home space never noticed");
    let (sources2, n) = rt.space_materialize_sources("boss", &s2).unwrap();
    assert_eq!(n, 0);
    let names: Vec<String> = fs::read_dir(&sources2)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n.ends_with("__quote-policy.md")), "unlinked mirror must prune: {names:?}");

    // Unlinking is NOT destroying: from the non-home space a real delete
    // is refused — deletion authority lives where the material lives.
    let err = rt.knowledge_delete_document(&t, &c, &s2, "boss", &doc).unwrap_err();
    assert!(err.to_string().contains("not found in the requested scope"), "{err}");
    assert_eq!(resolved(&rt, &s1, &doc), Some(1), "the refused delete changed nothing");

    // Relinking re-adopts the SAME material — history intact, follow again.
    rt.space_link_add("boss", &s2, &doc).unwrap();
    let mats = rt.space_materials("boss", &s2).unwrap();
    assert_eq!(mats.len(), 1);
    assert_eq!(mats[0].document_id, doc);
    assert_eq!(mats[0].binding, "follow");
    assert_eq!(mats[0].resolved_version_no, Some(1));
}
