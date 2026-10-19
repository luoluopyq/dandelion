//! G19: deep-building a knowledge space from its OWN published material
//! (`space_build_llm`), driven through the real runtime with a scripted
//! model. The store holds the material; the build reads evidence from a
//! one-way mirror the space regenerates before every build — never a third
//! storage. What only the full loop can prove:
//!
//! | rule | test |
//! |------|------|
//! | the build is an EDITOR action; non-members, Readers, ghosts, malformed ids and archived spaces are refused BY NAME before anything is materialized, and no denial echoes the space's content | `deep_build_gates_before_any_touch` |
//! | an Editor build materializes → registers the mirror → runs the UNCHANGED plan → articles → overview pipeline into the space's own data dir; members browse the result through `space_status`/`space_show` | `editor_deep_build_publishes_into_the_space` |
//! | the mirror is hash-idempotent, one-way (hand edits do not stick), and tracks the store: drafts never appear, a newly published doc appears, a new version replaces the file, a revoked version and a deleted document prune | `mirror_is_hash_idempotent_and_one_way` |
//! | a space with no published material is NAMED and refused, not built empty | `empty_space_is_named_and_refused` |
//! | the build honors the space's external policy: a `local_only` space is refused BY NAME with the remedy, before the mirror is touched and before any model turn | `a_local_only_space_is_refused_by_policy` |
//! | a space already bound to a REAL project workspace is refused with that path named — never hijacked onto the mirror | `a_space_bound_to_a_real_project_is_refused_not_hijacked` |
//! | cancelling a build that is not running answers `false` — the flag is not remembered, a stray click cannot kill the next build | `cancel_when_idle_is_honestly_false` |
//! | the profile override is Editor-gated, validates the domain exactly like `wiki_set_profile`, and reads back for Readers; no profile means the build decides from the material's shape | `profile_override_needs_editor_and_validates_domain` |

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki::{self, AI_OVERVIEW};
use dandelion_core::{KnowledgeRole, PermissionMode, Runtime, Settings};
use std::fs;
use std::sync::Arc;

fn runtime_at(data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    // Two articles keep the scripted turn count small while still exercising
    // plan → topics → overview ordering over the mirror.
    settings.wiki_build_max_articles = 2;
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

/// Tenant + customer + space minted through the real Store API (ids are
/// generated, never names or paths), owned by "boss" (publish needs Owner).
fn mint(rt: &Runtime, name: &str) -> (String, String, String) {
    let tenant = rt.knowledge_create_tenant("外贸事业部").unwrap();
    let customer = rt.knowledge_create_customer(&tenant.id, "外贸客户甲").unwrap();
    // `allow_summary`: deep build routes published material through the
    // configured model, so it refuses spaces whose policy forbids that
    // (see `a_local_only_space_is_refused_by_policy`).
    let space =
        rt.knowledge_create_space(&tenant.id, &customer.id, name, "allow_summary").unwrap();
    rt.knowledge_set_member(&space.id, "boss", KnowledgeRole::Owner).unwrap();
    (tenant.id, customer.id, space.id)
}

const DOC_A: &str = "报价折扣超过五个点须总监签字。\n三个点以内的折扣由销售自行决定。\n所有特价必须当日备案。\n";
const DOC_B: &str = "模具温度波动会导致缩水。\n保压时间不足会出现缩痕。\n周期缩短两成属于正常波动。\n";

/// One imported + published document, returned so callers can re-version,
/// revoke or delete it.
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

/// The mirror file for a source id (the document id prefixes it, so match
/// by the sanitized source suffix — names are deterministic, never guessed).
fn mirror_named(sources: &std::path::Path, suffix: &str) -> String {
    fs::read_dir(sources)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(suffix))
        .unwrap_or_else(|| panic!("no mirror file ending in {suffix} in {:?}", sources))
}

fn mirror_count(sources: &std::path::Path) -> usize {
    fs::read_dir(sources).unwrap().flatten().filter(|e| e.path().is_file()).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_build_gates_before_any_touch() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    // ONE turn staged: any model call at all would move complete_count.
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("不该被调用")]));
    let rt = runtime_at(&data, provider.clone());
    let (t, c, s) = mint(&rt, "报价规则空间");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    import_publish(&rt, &t, &c, &s, "secret", "暗中汇率条款由甲方单方解释。\n");

    // A Reader may READ the space but never PUBLISH a wiki over it. The
    // gate fires before materialization: the sources dir is never created,
    // and the denial never echoes the published text.
    let err = rt
        .space_build_llm("reader1", &s, false)
        .await
        .expect_err("publishing a wiki needs Editor");
    let msg = err.to_string();
    assert!(msg.contains("role does not permit"), "{msg}");
    assert!(!msg.contains("暗中汇率"), "the denial never echoes the material: {msg}");
    let sdir = wiki::space_data_dir(&data, &s);
    assert!(!sdir.join("sources").exists(), "a refused build must not touch the mirror");

    // Non-member: the same fail-closed denial as every other space surface.
    let err = rt.space_build_llm("ghost", &s, false).await.unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");

    // Identity and lifecycle wording, verbatim from the read surfaces.
    let err = rt.space_build_llm("boss", "space_ghost000", false).await.unwrap_err();
    assert!(err.to_string().contains("知识空间不存在"), "{err}");
    let err = rt.space_build_llm("boss", "../evil", false).await.unwrap_err();
    assert!(err.to_string().contains("形状非法"), "{err}");
    rt.knowledge_archive_space(&s, true).unwrap();
    let err = rt.space_build_llm("boss", &s, false).await.unwrap_err();
    assert!(err.to_string().contains("已归档"), "{err}");

    assert_eq!(provider.complete_count(), 0, "a gated build never reaches the model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editor_deep_build_publishes_into_the_space() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "注塑与报价知识库");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);
    import_publish(&rt, &t, &c, &s, "policy-b", DOC_B);

    // Materialize FIRST to learn the deterministic mirror names the plan
    // turn must cite — the build re-materializes idempotently itself.
    let (sources, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 2);
    let sdir = wiki::space_data_dir(&data, &s);
    assert_eq!(sources, sdir.join("sources"), "the mirror lives inside the space's own data dir");
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
    let rt2 = runtime_at(&data, provider.clone());
    let report = rt2.space_build_llm("boss", &s, false).await.expect("Editor build succeeds");

    // The SAME pipeline as a project build, one turn per step.
    assert_eq!(report.build, 1);
    assert!(!report.incremental);
    let t_quotes = wb::topic_file_for("报价折扣权限");
    let t_molding = wb::topic_file_for("注塑缺陷");
    assert_eq!(report.targets, vec![t_quotes.clone(), t_molding.clone(), AI_OVERVIEW.to_string()]);
    assert_eq!(report.published.len(), 3);
    assert_eq!(provider.complete_count(), 4, "plan + 2 articles + overview");

    // Published into the SPACE's data dir, and the mirror is now the
    // space's registered build workspace (one-way, store-derived).
    for f in [&t_quotes, &t_molding, AI_OVERVIEW] {
        assert!(wiki::compiled_dir(&sdir).join(f).is_file(), "{f} must be published");
    }
    assert!(wiki::status(&sdir).last_build_error.is_none());
    let store = dandelion_core::store::sqlite::Store::open(&data, 5000).unwrap();
    let (_, path) = store
        .knowledge_workspace_for_space(&s)
        .unwrap()
        .expect("the mirror is registered as the build workspace");
    assert_eq!(path, sources.to_string_lossy(), "the binding names the mirror, not a user path");

    // Members browse the result through the space-keyed reads, as with any
    // other space content.
    let status = rt2.space_status("reader1", &s).expect("Reader browses");
    for f in [&t_quotes, &t_molding, AI_OVERVIEW] {
        assert!(status.pages.iter().any(|p| &p.file == f), "{f} in the TOC");
    }
    let one = rt2.space_show("reader1", &s, Some(&t_quotes)).unwrap();
    assert!(one[0].body.contains("总监签字"), "{}", one[0].body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mirror_is_hash_idempotent_and_one_way() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (t, c, s) = mint(&rt, "版本跟踪空间");
    let (doc_a, ver_a1) = import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);

    // First materialize writes the file; the SAME hash must not rewrite it
    // (mtime proves it) — re-running before every build is free.
    let (sources, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 1);
    let file = sources.join(mirror_named(&sources, "__policy-a.md"));
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
    {
        let f = fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(old).unwrap();
    }
    rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), old, "an unchanged hash is not rewritten");

    // One-way mirror: a hand edit is RESTORED, never adopted.
    fs::write(&file, "有人在盘上改了镜像文件\n").unwrap();
    rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), DOC_A, "disk edits do not stick");

    // An unclaimed file (manual drop) is pruned — the mirror is exactly the
    // published set.
    fs::write(sources.join("leftover.md"), "不属于任何已发布文档\n").unwrap();
    rt.space_materialize_sources("boss", &s).unwrap();
    assert!(!sources.join("leftover.md").exists(), "unclaimed files are pruned");

    // A draft (imported, never published) is not evidence.
    rt.knowledge_import_document(&t, &c, &s, "boss", "note", "draft-x", "未发布的草稿内容。\n", 500)
        .unwrap();
    let (_, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 1, "drafts never reach the mirror");

    // A newly published document appears; a new published version replaces
    // the file's content (publishing v2 auto-revokes v1 in the store).
    let (_doc_b, _ver_b) = import_publish(&rt, &t, &c, &s, "policy-b", DOC_B);
    let (_, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 2);
    let (_doc_a2, ver_a2) =
        import_publish(&rt, &t, &c, &s, "policy-a", "报价折扣规则 v2：所有折扣一律备案。\n");
    rt.space_materialize_sources("boss", &s).unwrap();
    assert!(fs::read_to_string(&file).unwrap().contains("v2"), "the file tracks the published version");

    // Revoking the only live version prunes the document's mirror file…
    rt.knowledge_revoke_version(&t, &c, &s, "boss", &doc_a, &ver_a2).unwrap();
    let (_, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 1, "a revoked version stops being evidence");
    assert!(!file.exists(), "the pruned file is gone");
    let _ = ver_a1; // v1 was auto-revoked by the v2 publish — no resurrection.

    // …and so does deleting the document outright.
    rt.knowledge_delete_document(&t, &c, &s, "boss", &_doc_b).unwrap();
    let (_, n) = rt.space_materialize_sources("boss", &s).unwrap();
    assert_eq!(n, 0);
    assert_eq!(mirror_count(&sources), 0, "the mirror holds exactly the live published set");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_space_is_named_and_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let provider = Arc::new(ScriptedProvider::new(Vec::new()));
    let rt = runtime_at(&data, provider.clone());
    let (_t, _c, s) = mint(&rt, "空空间");
    let err = rt.space_build_llm("boss", &s, false).await.expect_err("nothing to build from");
    let msg = err.to_string();
    assert!(msg.contains("尚无已发布资料") && msg.contains("深度构建"), "{msg}");
    assert_eq!(provider.complete_count(), 0, "an empty space never reaches the model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_only_space_is_refused_by_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    // ONE turn staged: any model call at all would move complete_count.
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("不该被调用")]));
    let rt = runtime_at(&data, provider.clone());
    let (t, c, _) = mint(&rt, "报价规则空间");
    // A sibling space whose policy forbids cloud egress ("" ⇒ local_only).
    let strict = rt.knowledge_create_space(&t, &c, "本机限定空间", "").unwrap();
    rt.knowledge_set_member(&strict.id, "boss", KnowledgeRole::Owner).unwrap();
    import_publish(&rt, &t, &c, &strict.id, "policy-a", DOC_A);

    // Deep build routes material through the configured model, so the
    // space's own external policy refuses it — by name, with the remedy —
    // BEFORE the mirror is materialized and before any model turn.
    let err = rt
        .space_build_llm("boss", &strict.id, false)
        .await
        .expect_err("local_only must not be built with the cloud model");
    let msg = err.to_string();
    assert!(msg.contains("本机限定空间") && msg.contains("外部策略禁止云端模型"), "{msg}");
    assert!(msg.contains("允许云端摘要"), "the refusal names the remedy: {msg}");
    assert!(!wiki::space_data_dir(&data, &strict.id).join("sources").exists(),
        "a policy refusal must not touch the mirror");
    assert_eq!(provider.complete_count(), 0, "a policy refusal never reaches the model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_space_bound_to_a_real_project_is_refused_not_hijacked() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("real-project");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("README.md"), "# 真实项目\n").unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("不该被调用")]));
    let rt = runtime_at(&data, provider.clone());
    let (t, c, s) = mint(&rt, "已绑定项目的空间");
    import_publish(&rt, &t, &c, &s, "policy-a", DOC_A);

    // The space's build workspace is a REAL project directory (what the
    // migration registers for a project-backed space).
    let store = dandelion_core::store::sqlite::Store::open(&data, 5000).unwrap();
    store
        .knowledge_register_workspace(&s, &wiki::workspace_hash(&ws), &ws.to_string_lossy())
        .unwrap();

    let err = rt
        .space_build_llm("boss", &s, false)
        .await
        .expect_err("a bound space is refused, never re-pointed at the mirror");
    let msg = err.to_string();
    assert!(msg.contains("已绑定工作区") && msg.contains(&ws.to_string_lossy().to_string()), "{msg}");
    assert_eq!(provider.complete_count(), 0, "the refusal never reaches the model");
    let (_, path) = store.knowledge_workspace_for_space(&s).unwrap().expect("binding intact");
    assert_eq!(path, ws.to_string_lossy(), "the refusal changed nothing");
    assert!(!wiki::compiled_dir(&wiki::space_data_dir(&data, &s)).join(AI_OVERVIEW).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_when_idle_is_honestly_false() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (_t, _c, s) = mint(&rt, "取消语义空间");
    // Nothing is building: false, and the flag is NOT remembered — a stray
    // click cannot kill the next build.
    assert!(!rt.space_build_cancel("boss", &s).await.unwrap(), "idle cancel answers honestly");
    // The gate still runs: a non-member cannot even ask.
    let err = rt.space_build_cancel("ghost", &s).await.unwrap_err();
    assert!(err.to_string().contains("knowledge access denied"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_override_needs_editor_and_validates_domain() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(Vec::new())));
    let (_t, _c, s) = mint(&rt, "领域覆盖空间");
    rt.knowledge_set_member(&s, "reader1", KnowledgeRole::Reader).unwrap();
    rt.knowledge_set_member(&s, "editor1", KnowledgeRole::Editor).unwrap();

    // No saved profile: the build decides from the material's shape.
    assert!(rt.space_profile("reader1", &s).unwrap().is_none(), "fresh space has no override");

    // Saving is a mutation: Editor and up.
    let err = rt.space_set_profile("reader1", &s, &wiki::WikiProfile::generic()).unwrap_err();
    assert!(err.to_string().contains("role does not permit"), "{err}");
    rt.space_set_profile("editor1", &s, &wiki::WikiProfile::generic()).expect("Editor saves");
    let saved = rt.space_profile("reader1", &s).unwrap().expect("Reader reads back");
    assert_eq!(saved.domain, wiki::WikiProfile::DOMAIN_GENERIC);
    assert_eq!(saved.label, "资料与业务");

    // The domain validates exactly like wiki_set_profile.
    let bogus = wiki::WikiProfile { domain: "bogus".into(), label: "伪域".into() };
    let err = rt.space_set_profile("editor1", &s, &bogus).unwrap_err();
    assert!(err.to_string().contains("unknown wiki domain: bogus"), "{err}");
    assert_eq!(
        rt.space_profile("editor1", &s).unwrap().unwrap().domain,
        wiki::WikiProfile::DOMAIN_GENERIC,
        "the rejected write changed nothing"
    );
    rt.space_set_profile("editor1", &s, &wiki::WikiProfile::code()).expect("switch to code");
    assert!(rt.space_profile("editor1", &s).unwrap().unwrap().is_code());

    // The gate runs first: a ghost space is denied, not saved.
    let err = rt.space_set_profile("editor1", "space_ghost000", &wiki::WikiProfile::code()).unwrap_err();
    assert!(err.to_string().contains("知识空间不存在"), "{err}");
}
