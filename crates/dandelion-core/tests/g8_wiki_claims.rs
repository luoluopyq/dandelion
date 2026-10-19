//! G8 (spec 三): citation↔conclusion binding at the whole-loop level.
//! A citation is evidence for ONE sentence, never for the article: a real
//! but never-shown line gets RE-EVIDENCED first (the exact claimed range is
//! re-read from disk, byte-capped, audit persisted), and only what STILL
//! stands on an unverifiable citation is demoted to 【待核实】 — the claim
//! itself loses factual status, not just its marker. One legal citation
//! rescues its own sentence and never the article; one bad citation sinks
//! its own sentence and never the article.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki;
use dandelion_core::{PermissionMode, Runtime, Settings};
use std::fs;
use std::sync::Arc;

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{\"name\":\"app\",\"main\":\"src/run.ts\"}");
    touch(&ws, "src/run.ts", "export function launch() {\n  boot();\n}\n");
    touch(&ws, "src/a1.ts", "export const A = 1;\n");
    touch(&ws, "src/zeta_last.ts", "export function boot() {\n  run();\n}\n");
    touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
    // 60 numbered lines: L50-L52 really exist, but no pack window for
    // 快速上手 can ever have shown them — a claim there MUST trigger
    // re-evidence, never silent pass and never silent demotion. Placement
    // matters: `sanitize_inputs` prepends the TOP-3 code files (spec 五
    // grounding) to a doc-only topic, and a ≤160-line model input renders
    // WHOLE — so this file must sit past the cut (run, a1, zeta_last win
    // it) or L50-L52 would be legitimately shown and nothing to re-evidence.
    let deep: String = (1..=60).map(|n| format!("line {n}\n")).collect();
    touch(&ws, "src/zz_deep.ts", &deep);
    fs::create_dir_all(&data).unwrap();
    let pdir = resolved(&data, &ws);
    (tmp, data, ws, pdir)
}

/// The space-resolved Wiki home — the same resolution the runtime uses
/// (post phase-2 migration a registered workspace lives under
/// `wiki/spaces/<id>`, so assertions must read the resolved dir too).
fn resolved(data: &std::path::Path, ws: &std::path::Path) -> std::path::PathBuf {
    let store = dandelion_core::store::sqlite::Store::open(data, 5000).unwrap();
    dandelion_core::wiki_space_migrate::ensure_space_dir(&store, data, ws, true).unwrap()
}

fn runtime_at(
    data: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    tune: impl FnOnce(&mut Settings),
) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.wiki_build_max_articles = 2;
    tune(&mut settings);
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

fn plan_turn() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么把项目跑起来\",\"inputs\":[\"docs/guide.md\"]},\
         {\"title\":\"核心流程\",\"focus\":\"一次典型操作\",\"inputs\":[\"src/zeta_last.ts\"]}]\n```",
    )
}

fn cited(body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `docs/guide.md:L1-L3`。\n"))
}

fn t_quick() -> String {
    wb::topic_file_for("快速上手")
}

/// Legal cite for its own sentence; a claim on the real-but-unshown deep
/// window for another. The two sentences must be judged INDEPENDENTLY.
fn claim_deep() -> ScriptedTurn {
    ScriptedTurn::text(
        "先读入口文档 `docs/guide.md:L1-L3`。深层逻辑见 `src/zz_deep.ts:L50-L52`。",
    )
}

fn refetch_audit(pdir: &std::path::Path, round: u32) -> std::path::PathBuf {
    let stem = t_quick().strip_suffix(".md").unwrap().to_string();
    wiki::wiki_dir(pdir).join("builds").join("refetch").join(format!("{stem}-r{round}.md"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_unshown_claim_is_refetched_from_disk_before_anything_is_demoted() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        claim_deep(),                      // first pass: unshown deep claim
        claim_deep(),                      // strict retry: same claim stands
        ScriptedTurn::text(                // on re-read evidence: now provable
            "先读入口文档 `docs/guide.md:L1-L3`。深层逻辑见 `src/zz_deep.ts:L50-L52`。",
        ),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("build survives");

    assert_eq!(provider.complete_count(), 6, "plan + article + strict retry + refetch retry + flow + overview");
    assert_eq!(report.refetch_rounds, 1, "one re-evidence round ran");
    assert_eq!(report.evidence_retries, 1);
    assert_eq!(report.conclusion_demotions, 0, "re-evidenced ⇒ NOTHING demoted: {:?}", report.warnings);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let top = fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    assert!(top.contains("`src/zz_deep.ts:L50-L52`"), "the claim publishes as a live cite: {top}");
    assert!(!top.contains("待核实"), "{top}");

    // The round is auditable on disk after the build, in the durable
    // builds dir (staging gets wiped; an audit nobody can open is none).
    // The audit file IS the pack that went into the retry prompt (same
    // string), so this proves the model was re-evidenced with the
    // verbatim re-read of EXACTLY the claimed window.
    let audit = fs::read_to_string(refetch_audit(&pdir, 1)).expect("refetch audit persisted");
    assert!(audit.contains("已逐字回读"), "{audit}");
    assert!(audit.contains("50: line 50") && audit.contains("52: line 52"), "{audit}");
    assert!(!audit.contains("59: line 59"), "only what was claimed is re-read: {audit}");
    // And the publish gate accepted the extended evidence window: the
    // article shipped citing lines only the re-read ever showed.
    assert!(report.published.iter().any(|f| f == &t_quick()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refetch_disabled_demotes_only_the_unverified_sentence() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        claim_deep(),
        claim_deep(),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    // Budget of zero re-read rounds: straight to demotion.
    let rt = runtime_at(&data, provider.clone(), |s| s.wiki_evidence_refetch_max = 0);
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("demotion saves the build");

    assert_eq!(provider.complete_count(), 5);
    assert_eq!(report.refetch_rounds, 0);
    assert_eq!(report.conclusion_demotions, 1, "exactly ONE sentence loses factual status");
    assert!(!refetch_audit(&pdir, 1).exists(), "no round ran ⇒ no audit");

    let top = fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    // Per-SENTENCE gating, provable: the legal-cite sentence stands as
    // fact with no marker…
    assert!(top.starts_with("先读入口文档 `docs/guide.md:L1-L3`。"), "legal sentence untouched: {top}");
    // …while the unverified one is demoted, token gone, no `:L` left.
    assert_eq!(top.matches("【待核实】").count(), 1, "{top}");
    assert!(top.contains("（引用待核实：src/zz_deep.ts）"), "{top}");
    assert!(!top.contains("L50"), "fabricated-range cite does not publish: {top}");
    // One legal citation did NOT rescue the article's other claim;
    // one bad citation did NOT sink the article.
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("待核实"), "{}", report.warnings[0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refetch_budget_spent_still_bad_claim_demotes_alone_good_stands() {
    let (_t, data, ws, pdir) = setup();
    // After the re-read the model fixes L50-L52 but invents L40-L41 in a
    // separate sentence — and the refetch budget (1) is spent. The fixed
    // claim must PUBLISH as fact; only the fresh bad one demotes.
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        claim_deep(),
        claim_deep(),
        ScriptedTurn::text(
            "先读入口文档 `docs/guide.md:L1-L3`。深层逻辑见 `src/zz_deep.ts:L50-L52`。另见 `src/zz_deep.ts:L40-L41`。",
        ),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("build survives");

    assert_eq!(provider.complete_count(), 6, "exactly one refetch round");
    assert_eq!(report.refetch_rounds, 1, "budget is 1 round — not more, not less");
    assert_eq!(report.conclusion_demotions, 1);

    let top = fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    assert!(top.contains("深层逻辑见 `src/zz_deep.ts:L50-L52`。"), "re-evidenced claim stands: {top}");
    assert_eq!(top.matches("【待核实】").count(), 1, "only the fresh bad sentence: {top}");
    assert!(top.contains("（引用待核实：src/zz_deep.ts）") && !top.contains("L40"), "{top}");
}
