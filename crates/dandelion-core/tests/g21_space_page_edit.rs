//! G21: the space reading tab's WRITE paths, through the real runtime.
//!
//! Manual authoring (`space_page_save`) and policy management
//! (`space_set_policy`) plus the §九 publish-time authorization recheck —
//! the half of 「构建固定输入快照，发布前重新检查版本和授权」 that the
//! start gate alone cannot cover, because a deep build is long and the
//! policy can flip while the model is still writing.
//!
//! | rule | test |
//! |------|------|
//! | saving a page is an EDITOR action on the SAME gates as every space surface (roles, ghosts, malformed ids, archived); page names are validated for shape, content for size, and the refusal names the rule | `page_save_gates_are_the_space_gates` |
//! | a hand page in a NEVER-BUILT space is readable the instant it lands (TOC + direct fetch), carries no machine protection (nothing to protect against) and lives on the legacy layout (version 0) | `hand_page_in_a_never_built_space_is_immediately_readable` |
//! | editing a machine-built page PROTECTS it — `user_edited` set at save time; the next full build lists the file in `skipped_user_edited`, keeps the human body in `compiled/` and files the machine text as a pending conflict | `editing_a_machine_page_protects_it_and_the_next_build_files_it_as_pending` |
//! | a policy flip MID-BUILD stops the publish, not the model: all turns are spent, staging is written, then the recheck refuses BY NAME; the pointer never moves and the previous version stays complete and readable — and flipping back un-refuses (the gate is not sticky) | `publish_refuses_when_policy_flips_mid_build` |
//! | `space_set_policy` is Owner-only, validates the value, and takes effect on the very next decision — a space flipped to `local_only` is refused at the very NEXT build start | `space_set_policy_is_owner_and_takes_effect_immediately` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki;
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

fn mint(rt: &Runtime, name: &str) -> (String, String, String) {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    let space =
        rt.knowledge_create_space(&tenant.id, &customer.id, name, "allow_summary").unwrap();
    rt.knowledge_set_member(&space.id, "boss", KnowledgeRole::Owner).unwrap();
    (tenant.id, customer.id, space.id)
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

fn mirror_named(sources: &std::path::Path, suffix: &str) -> String {
    fs::read_dir(sources)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(suffix))
        .unwrap_or_else(|| panic!("no mirror file ending in {suffix} in {:?}", sources))
}

/// The four scripted turns of the two-article deep build over `fa`/`fb`,
/// staged exactly like g19 — one set per build attempt, so
/// `complete_count` counts model work honestly per runtime.
fn build_turns(fa: &str, fb: &str) -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::text(format!(
            "```json\n[{{\"title\":\"报价折扣权限\",\"focus\":\"谁能批多少折扣\",\"inputs\":[\"{fa}\"]}},\
             {{\"title\":\"注塑缺陷\",\"focus\":\"缩水与缩痕的成因\",\"inputs\":[\"{fb}\"]}}]\n```"
        )),
        ScriptedTurn::text(format!("五个点以上须总监签字，特价须备案。\n依据 `{fa}:L1-L2`。\n")),
        ScriptedTurn::text(format!("温度波动出缩水，保压不足出缩痕。\n依据 `{fb}:L1-L3`。\n")),
        ScriptedTurn::text(format!(
            "本库覆盖报价与注塑两类资料。依据 `{fb}:L2-L2`。## 学习顺序\n1. [报价折扣权限](TOPIC-1.md)\n2. [注塑缺陷](TOPIC-2.md)"
        )),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_save_gates_are_the_space_gates() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    // No turns at all: every rejection below must return before the
    // runtime could even think about the model.
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "报价规则空间");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);

    // Saving a page PUBLISHES content — Editor and up, the same ledger
    // gate the ledger review uses. A Reader reads, never writes.
    let err = rt.space_page_save("reader1", &s, "话术", "客户问交期先查船期。").unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    let err = rt.space_page_save("ghost", &s, "话术", "x").unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");
    let err = rt.space_page_save("boss", "space_ghost000", "话术", "x").unwrap_err();
    assert!(err.to_string().contains("知识空间不存在"), "{err}");

    // Name shape: identity is ONE normalized file name inside the space's
    // own data dir — traversal is refused BY NAME, never sanitized into a
    // different file. Empty and path-y names all land on the same rule.
    for bad in ["../evil", "a/b", "a\\b", "   "] {
        let err = rt
            .space_page_save("boss", &s, bad, "不该被回显的内容")
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("页面名非法"), "{err}");
        assert!(
            !msg.contains("不该被回显"),
            "the refusal names the rule, not the content: {msg}"
        );
    }

    // Size cap: the SAME ceiling as a ledger proposal — one entry point
    // must not smuggle past what the other enforces.
    let max = rt.settings().wiki_proposal_max_bytes;
    let big = "字".repeat(max / 3 + 1); // 3 bytes per char in UTF-8
    assert!(big.len() > max);
    let err = rt.space_page_save("boss", &s, "巨页", &big).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("超出上限") && msg.contains(&max.to_string()), "{msg}");

    // Archived spaces take ALL writes, pages included.
    rt.knowledge_archive_space(&s, true).unwrap();
    let err = rt.space_page_save("boss", &s, "话术", "x").unwrap_err();
    assert!(err.to_string().contains("已归档"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hand_page_in_a_never_built_space_is_immediately_readable() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (_t, _c, s) = mint(&rt, "话术空间");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    let sdir = wiki::space_data_dir(&data, &s);
    assert!(wiki::current_pointer(&sdir).is_none(), "a fresh space has no versioned wiki");

    // Hand-authoring a page never waits on a machine build: the first page
    // of a never-built space is the FIRST published page, legacy layout.
    let out = rt
        .space_page_save("boss", &s, "常用话术", "客户问交期，先确认船期再回复。\n")
        .expect("hand page saves");
    assert_eq!(out.file, "常用话术.md", "the name is core-normalized (补 .md)");
    assert!(!out.protected, "a hand page has no machine record to protect against");
    assert_eq!(out.version, 0, "no pointer yet: the atomic write IS the commit");
    assert_eq!(
        rt.space_page_save("boss", &s, "常用话术.md", "改：先答交期再补船期。\n").unwrap().file,
        "常用话术.md",
        "a name that already ends in .md is not double-suffixed"
    );

    // Immediately readable through BOTH space reads, by any member.
    let status = rt.space_status("reader1", &s).expect("Reader browses");
    assert!(status.pages.iter().any(|p| p.file == "常用话术.md"), "{:?}", status.pages);
    let one = rt.space_show("reader1", &s, Some("常用话术.md")).unwrap();
    assert_eq!(one.len(), 1);
    assert!(one[0].body.contains("先答交期再补船期"), "{}", one[0].body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_a_machine_page_protects_it_and_the_next_build_files_it_as_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "注塑与报价知识库");
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);
    import_publish(&rt, &t, &c, &s, "policy-b", DOC_B);
    let (sources, _n) = rt.space_materialize_sources("boss", &s).unwrap();
    let fa = mirror_named(&sources, "__policy-a.md");
    let fb = mirror_named(&sources, "__policy-b.md");
    let sdir = wiki::space_data_dir(&data, &s);

    let provider = Arc::new(ScriptedProvider::new(build_turns(&fa, &fb)));
    let rt2 = runtime_at(&data, provider.clone());
    let first = rt2.space_build_llm("boss", &s, false).await.expect("v1 build");
    assert_eq!(first.published.len(), 3);
    let t_quotes = wb::topic_file_for("报价折扣权限");
    assert!(!wb::load_articles(&sdir).articles[&t_quotes].user_edited);

    // A human fixes one sentence. The save PROTECTS the page: the
    // `user_edited` mark lands before the version flip, so even a crash
    // between them cannot let the next build overwrite the human body.
    let human = "五个点以上须总监签字；三个点以内销售自批并报总监周知。\n";
    let out = rt2.space_page_save("boss", &s, &t_quotes, human).expect("edit saves");
    assert!(out.protected, "editing a machine-built page arms its protection");
    assert!(wb::load_articles(&sdir).articles[&t_quotes].user_edited, "protection is on disk");
    // The edit itself publishes: readers get the human words through the
    // normal pointer, not a stale snapshot.
    let shown = rt2.space_show("boss", &s, Some(&t_quotes)).unwrap();
    assert_eq!(shown[0].body, human);

    // The next FULL rebuild re-plans the same titles (the machine WOULD
    // rewrite this page) and must skip it, keeping the human body in
    // compiled/ and parking its own text in the pending conflicts.
    let provider2 = Arc::new(ScriptedProvider::new(build_turns(&fa, &fb)));
    let rt3 = runtime_at(&data, provider2.clone());
    let second = rt3.space_build_llm("boss", &s, false).await.expect("rebuild succeeds");
    assert!(
        second.skipped_user_edited.contains(&t_quotes),
        "protected page must be listed: {:?}",
        second.skipped_user_edited
    );
    assert!(!second.published.contains(&t_quotes), "protected page is not published");
    assert!(second.pending.contains(&t_quotes), "machine text files as pending conflict");
    let body = fs::read_to_string(wiki::compiled_dir(&sdir).join(&t_quotes)).unwrap();
    assert_eq!(body, human, "the human body survives the rebuild");
    let copy = fs::read_to_string(wb::pending_dir(&sdir).join(&t_quotes)).expect("pending copy");
    assert!(copy.contains("总监签字") && !copy.contains("周知"), "machine text waits pending: {copy}");
    assert_eq!(provider2.complete_count(), 4, "the build still did its model work");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_refuses_when_policy_flips_mid_build() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "报价规则空间");
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);
    import_publish(&rt, &t, &c, &s, "policy-b", DOC_B);
    let (sources, _n) = rt.space_materialize_sources("boss", &s).unwrap();
    let fa = mirror_named(&sources, "__policy-a.md");
    let fb = mirror_named(&sources, "__policy-b.md");
    let sdir = wiki::space_data_dir(&data, &s);

    // v1 publishes while the policy allows cloud summaries.
    let provider = Arc::new(ScriptedProvider::new(build_turns(&fa, &fb)));
    let rt2 = runtime_at(&data, provider.clone());
    let first = rt2.space_build_llm("boss", &s, false).await.expect("v1 build");
    assert_eq!(first.published.len(), 3);
    assert!(wiki::current_pointer(&sdir).is_some(), "v1 left a version pointer");
    let t_quotes = wb::topic_file_for("报价折扣权限");
    let body_v1 = fs::read_to_string(wiki::compiled_dir(&sdir).join(&t_quotes)).unwrap();

    // The Owner flips the policy WHILE a build could still be running.
    // Calling the raw build directly (as a build already IN FLIGHT is,
    // past its start gate) proves the publish recheck on its own — the
    // start gate of `space_build_llm` would refuse earlier.
    rt2.space_set_policy("boss", &s, "local_only").unwrap();
    let provider2 = Arc::new(ScriptedProvider::new(build_turns(&fa, &fb)));
    let rt3 = runtime_at(&data, provider2.clone());
    let err = rt3
        .wiki_build_llm(Some(&sdir.join("sources")), false)
        .await
        .expect_err("a flipped policy stops the publish");
    let msg = err.to_string();
    assert!(msg.contains("不予发布"), "{msg}");
    assert!(msg.contains("外部策略已禁止云端模型"), "{msg}");
    assert!(msg.contains("报价规则空间"), "the refusal names the space: {msg}");
    assert_eq!(provider2.complete_count(), 4, "the refusal lands AFTER all model work, at publish");

    // Nothing readable moved. A build re-snapshots and carries the CURRENT
    // tree forward at its start (that is how the pointer can advance
    // harmlessly before any model work), so what §九 protects is the
    // content: the refused model text stayed in staging, and readers are
    // still served the v1 bodies byte-for-byte.
    assert_eq!(fs::read_to_string(wiki::compiled_dir(&sdir).join(&t_quotes)).unwrap(), body_v1);
    assert_eq!(rt3.space_show("boss", &s, Some(&t_quotes)).unwrap()[0].body, body_v1);
    // The refusal is not silent: status carries the failed stage.
    let st = rt3.space_status("boss", &s).unwrap();
    let rec = st.last_build_error.expect("failed publish is recorded");
    assert_eq!(rec.stage, "authz", "{rec:?}");
    assert!(rec.reason.contains("不予发布"), "{rec:?}");

    // Flipping back un-refuses — the gate reads the CURRENT policy every
    // time and remembers nothing.
    rt3.space_set_policy("boss", &s, "allow_summary").unwrap();
    let provider3 = Arc::new(ScriptedProvider::new(build_turns(&fa, &fb)));
    let rt4 = runtime_at(&data, provider3.clone());
    let third = rt4
        .wiki_build_llm(Some(&sdir.join("sources")), false)
        .await
        .expect("a re-allowed policy publishes again");
    assert_eq!(third.published.len(), 3);
    assert!(third.build > first.build, "the re-allowed build actually published a new build");
    assert!(wiki::status(&sdir).last_build_error.is_none(), "success clears the recorded failure");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn space_set_policy_is_owner_and_takes_effect_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "报价规则空间");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    rt.knowledge_set_member(&s, "ed1", KnowledgeRole::Editor).unwrap();
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);

    // Granting cloud egress (or revoking it) is a management act: Owner
    // only. The author who writes inside the policy cannot move it.
    let err = rt.space_set_policy("reader1", &s, "deny_cloud").unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    let err = rt.space_set_policy("ed1", &s, "deny_cloud").unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    let err = rt.space_set_policy("ghost", &s, "deny_cloud").unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");
    let err = rt.space_set_policy("boss", &s, "banana").unwrap_err();
    assert!(err.to_string().contains("invalid external policy"), "{err}");

    // Owner flips it; the verdict consumers re-read the stored row, so
    // the NEXT build start is already refused by name with the remedy —
    // the flip needs no rebuild, no re-register, no restart.
    rt.space_set_policy("boss", &s, "local_only").unwrap();
    let store = dandelion_core::store::sqlite::Store::open(&data, 5000).unwrap();
    let d = store.knowledge_external_policy_recheck(&s, "cloud_summary").unwrap();
    assert!(!d.allowed && d.policy == "local_only", "{d:?}");
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("不该被调用")]));
    let rt2 = runtime_at(&data, provider.clone());
    let err = rt2.space_build_llm("boss", &s, false).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("外部策略禁止云端模型"), "{msg}");
    assert!(msg.contains("允许云端摘要"), "the refusal carries the remedy: {msg}");
    assert_eq!(provider.complete_count(), 0, "a start-gate refusal never reaches the model");

    // And back: local retrieval was never the point of contention, cloud
    // work is allowed again on the very next decision.
    rt.space_set_policy("boss", &s, "allow_summary").unwrap();
    let d = store.knowledge_external_policy_recheck(&s, "cloud_summary").unwrap();
    assert!(d.allowed && d.policy == "allow_summary", "{d:?}");
}
