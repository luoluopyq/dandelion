//! G6: publish consistency and cancellation (spec 四 + 一.3 + 二.1 + 二.4).
//! The deep build must be safe exactly where the old path was not:
//! - a source file that moves WHILE the build runs blocks publication —
//!   articles must never ship pointing at evidence that already changed;
//! - a failure in the write phase rolls every touched file and both ledgers
//!   back to the previous published state (recoverable version switch, not a
//!   half-written Wiki);
//! - cancel lands mid-model-turn and at step boundaries, records stage
//!   "cancel", keeps the previous build readable, and a stray cancel that
//!   races no build never kills the NEXT build;
//! - `wiki_coverage_for` gives the desktop the honest scan story read-only,
//!   and `status` flags model articles whose evidence has moved;
//! - a human-edited article rides through every later build untouched
//!   (skipped on publish, its slot never re-planned, its record preserved).

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use dandelion_core::wiki::{self, AI_OVERVIEW};
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
    touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
    // Enough surface that two topics can stand on DISJOINT evidence — the
    // staleness test below needs one article that never saw `src/run.ts`.
    touch(&ws, "docs/step-one.md", "快速上手第一步\n");
    touch(&ws, "docs/step-two.md", "怎么跑起来\n");
    touch(&ws, "docs/step-three.md", "第三步\n");
    touch(&ws, "docs/core-flow.md", "核心流程走一遍\n");
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

fn runtime_at(data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.wiki_build_max_articles = 2;
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider).expect("runtime")
}

// Topic 1 declares its OWN source file (`src/a1.ts`) beside its docs. A
// doc-only nomination would now receive top-code grounding from
// `sanitize_inputs` (spec 五) — which necessarily includes the entry
// `src/run.ts` — and the staleness test below needs one article that
// never saw `src/run.ts`. Declaring its own code keeps the evidence
// disjoint honestly: the sanitizer sees code already present and stays
// hands off, so `src/run.ts` still never appears in this pack.
fn plan_turn() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么跑起来\",\"inputs\":[\"docs/guide.md\",\"docs/step-one.md\",\"docs/step-two.md\",\"docs/step-three.md\",\"src/a1.ts\"]},\
         {\"title\":\"核心流程\",\"focus\":\"典型操作\",\"inputs\":[\"src/run.ts\",\"src/a1.ts\",\"package.json\"]}]\n```",
    )
}

fn cited(body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `docs/guide.md:L1-L3`。\n"))
}

/// An article turn whose citation matches its topic's OWN pack: topic 2
/// stands on the entry sources (its pack never shows the docs), and the
/// overview stands on the root manifest. Citing evidence the pack never
/// displayed is exactly the fabricated proof the shown-range publish gate
/// now refuses — the script must be honest to exercise the happy path.
fn cited_src(body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `src/run.ts:L1-L3`。\n"))
}

fn cited_manifest(body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `package.json:L1-L1`。\n"))
}

/// Title-stable article files, computed from the plan's titles.
fn t_quick() -> String {
    wb::topic_file_for("快速上手")
}

fn t_flow() -> String {
    wb::topic_file_for("核心流程")
}

fn staged(file: &str, title: &str, content: &str) -> wb::StagedArticle {
    wb::StagedArticle {
        file: file.into(),
        title: title.into(),
        focus: String::new(),
        content: content.into(),
        inputs: vec!["docs/guide.md".into()],
        ranges: Vec::new(),
    }
}

// ---------------------------------------------------------------- 四.2 drift

#[test]
fn publish_refuses_when_source_moved_after_the_evidence_was_taken() {
    let (_t, _data, ws, pdir) = setup();
    let index = wi::build_index(&pdir, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &pdir,
        &index,
        &[staged("TOPIC-1.md", "快速上手", "第一版内容，依据 `docs/guide.md:L1-L3`。\n")],
    )
    .unwrap();
    let before = fs::read(wiki::compiled_dir(&pdir).join("TOPIC-1.md")).unwrap();

    // The source moves while the build is still reasoning over the old text.
    touch(&ws, "docs/guide.md", "# Guide v2\n\nthe contract changed\n");
    let err = wb::publish_build(
        &pdir,
        &index, // the stale index the in-flight build still believes in
        &[staged("TOPIC-1.md", "快速上手", "第二版内容，依据 `docs/guide.md:L1-L3`。\n")],
    )
    .expect_err("drifted evidence must block publication");
    let msg = err.to_string();
    assert!(msg.contains("构建期间源码已变化"), "honest reason: {msg}");
    assert!(msg.contains("docs/guide.md"), "the drifted file is named: {msg}");

    // Nothing was written: old article bytes and registry version survive.
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join("TOPIC-1.md")).unwrap(), before);
    assert_eq!(wb::load_articles(&pdir).version, 1);
}

// -------------------------------------------------------------- 四.1 rollback

#[test]
fn write_phase_failure_rolls_everything_back_to_the_previous_version() {
    let (_t, _data, ws, pdir) = setup();
    let index = wi::build_index(&pdir, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &pdir,
        &index,
        &[
            staged("TOPIC-1.md", "甲", "第一版甲，依据 `docs/guide.md:L1-L3`。\n"),
            staged("TOPIC-2.md", "乙", "第一版乙，依据 `docs/guide.md:L1-L3`。\n"),
        ],
    )
    .unwrap();
    let a1 = fs::read(wiki::compiled_dir(&pdir).join("TOPIC-1.md")).unwrap();
    let a2 = fs::read(wiki::compiled_dir(&pdir).join("TOPIC-2.md")).unwrap();

    // The page registry's path is a DIRECTORY, so the LAST visible write of
    // the apply phase is guaranteed to fail — after the bodies were written
    // into the fresh (still invisible) snapshot and articles.json rewritten.
    fs::remove_file(wiki::registry_path(&pdir)).unwrap();
    fs::create_dir(wiki::registry_path(&pdir)).unwrap();
    let version_before = wiki::current_pointer(&pdir).unwrap().version;
    let err = wb::publish_build(
        &pdir,
        &index,
        &[
            staged("TOPIC-1.md", "甲", "第二版甲，依据 `docs/guide.md:L1-L3`。\n"),
            staged("TOPIC-2.md", "乙", "第二版乙，依据 `docs/guide.md:L1-L3`。\n"),
            staged("TOPIC-3.md", "丙", "第二版丙，依据 `docs/guide.md:L1-L3`。\n"),
        ],
    )
    .expect_err("a failing visible write must fail the publish");
    assert!(
        matches!(err, dandelion_core::Error::Io(_)),
        "the registry write failure surfaces honestly: {err}"
    );

    // Versioned commit, failed: the pointer never moved, so the whole visible
    // Wiki is still v1 byte-for-byte, the ledger never left version 1, and
    // the half-built snapshot plus its journal were removed — no orphans.
    assert_eq!(wiki::current_pointer(&pdir).unwrap().version, version_before);
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join("TOPIC-1.md")).unwrap(), a1);
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join("TOPIC-2.md")).unwrap(), a2);
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.version, 1);
    assert_eq!(reg.articles["TOPIC-1.md"].build, 1);
    assert!(!reg.articles.contains_key("TOPIC-3.md"));
    assert!(!wb::journal_path(&pdir).exists(), "journal cleaned up by the abort");
    let versions: Vec<String> = fs::read_dir(wiki::versions_root(&pdir))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(versions, vec![version_before.to_string()], "the orphan snapshot is gone");
}

// ------------------------------------------------------------------- 一.3 cancel

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_interrupts_the_model_turn_keeps_v1_and_never_poisons_the_next_build() {
    let (_t, data, ws, pdir) = setup();
    // v1 published normally, so cancellation has something to preserve.
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("V1 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, good);
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    let v1 = fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap();

    // A stray cancel with no build running must be a no, and must store
    // nothing that could kill the build that starts right after.
    assert!(!wb::request_cancel(&pdir), "no build → nothing to cancel");

    // v2: the plan turn hangs until cancelled.
    let hanging = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::hang_until_cancel()]));
    let rt2 = runtime_at(&data, hanging.clone());
    let build = {
        let rt2 = rt2.clone();
        let ws = ws.clone();
        tokio::spawn(async move { rt2.wiki_build_llm(Some(&ws), false).await })
    };
    let mut spins = 0u32;
    while !wb::is_building(&pdir) && spins < 250_000 {
        tokio::task::yield_now().await;
        spins += 1;
    }
    assert!(wb::is_building(&pdir), "the spawned build must mark itself building");
    // Interrupt mid-turn. A human clicks cancel after the turn is visibly
    // live; the test must interrupt a turn that has actually BEGUN. (Cancelling
    // in the sliver between turn registration and the run taking its session
    // slot is a real but vanishingly narrow race, and no harness can
    // deterministically hit it; waiting for the provider call itself removes
    // the flake without weakening what is asserted.)
    let mut guard = 0u32;
    while hanging.complete_count() == 0 && guard < 20_000 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        guard += 1;
    }
    assert_eq!(hanging.complete_count(), 1, "the plan turn must be IN FLIGHT before cancel");
    assert!(!wb::active_turn_sessions(&pdir).is_empty(), "the running build must expose its model turn");
    assert!(rt2.wiki_build_cancel(Some(&ws)).await, "cancel reaches the live build");

    let err = build.await.expect("task").expect_err("cancelled build must end in error");
    assert!(err.to_string().contains("取消"), "honest cancel error: {err}");
    assert!(
        wb::active_turn_sessions(&pdir).is_empty(),
        "an interrupted build must leave NO tracked model turn behind — \
         residue would make a later cancel fire at an unrelated session"
    );
    let e = wiki::status(&pdir).last_build_error.expect("cancel is recorded with its stage");
    assert_eq!(e.stage, "cancel", "desktop can show exactly where it stopped");
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap(), v1, "v1 stays readable");
    assert_eq!(wb::load_articles(&pdir).version, 1);

    // The next build starts clean: no poisoned flag, slot free, publishes.
    let later = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档（第二版）。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("V2 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt3 = runtime_at(&data, later);
    let report = rt3.wiki_build_llm(Some(&ws), false).await.expect("next build must run");
    assert_eq!(report.build, 2);
    assert!(wiki::status(&pdir).last_build_error.is_none(), "a success clears the old failure");
}

// ---------------------------------------------------------------- 六.4 concurrency

/// A runtime like `runtime_at` but with an explicit article concurrency.
fn runtime_conc(data: &std::path::Path, provider: Arc<ScriptedProvider>, conc: usize) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.wiki_build_max_articles = 2;
    settings.wiki_build_concurrency = conc;
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider).expect("runtime")
}

/// A plan whose two topics BOTH stand on `package.json` (plus disjoint other
/// evidence). Under `wiki_build_concurrency = 2` the two article turns pop
/// their scripted replies in arrival order, which no test may assume — an
/// evidence overlap both packs contain makes every turn's citation valid
/// whichever order the pool delivers them in.
fn plan_turn_overlap() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么跑起来\",\"inputs\":[\"docs/guide.md\",\"docs/step-one.md\",\"docs/step-two.md\",\"package.json\"]},\
         {\"title\":\"核心流程\",\"focus\":\"典型操作\",\"inputs\":[\"src/run.ts\",\"src/a1.ts\",\"package.json\",\"docs/core-flow.md\"]}]\n```",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_build_covers_every_target_and_reports_elapsed_and_calls() {
    let (_t, data, ws, pdir) = setup();
    // Plan + two topic turns (delivered in ANY order) + the overview, which
    // runs last and alone so it can link the finished topic articles.
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn_overlap(),
        cited_manifest("两篇主题文章并行产出。"),
        cited_manifest("两篇主题文章并行产出。"),
        cited_manifest("并行构建的概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_conc(&data, good.clone(), 2);
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("concurrent build publishes");

    // Determinism survives parallelism: the report lists targets in PLAN
    // order and publishes in the same order the serial build would.
    assert_eq!(report.targets, vec![t_quick(), t_flow(), AI_OVERVIEW.to_string()], "plan order, never completion order: {report:?}");
    assert_eq!(report.published, vec![t_quick(), t_flow(), AI_OVERVIEW.to_string()]);
    assert!(report.warnings.is_empty(), "honest scripts need no repair: {:?}", report.warnings);
    // Telemetry (spec 六): exactly four model turns — plan, two articles,
    // overview — no phantom extra calls from the pool, no retry hidden.
    assert_eq!(report.model_calls, 4, "plan + 2 topics + overview, nothing else: {report:?}");
    assert_eq!(good.history().len(), 4, "the provider saw exactly four requests");
    assert!(report.elapsed_ms > 0, "the build reports what it cost: {report:?}");
    // The bytes on disk are the same complete set — a concurrent pool that
    // raced itself would show up here as a missing or half-written article.
    for f in [t_quick(), t_flow(), AI_OVERVIEW.to_string()] {
        let body = fs::read_to_string(wiki::compiled_dir(&pdir).join(&f)).expect("published article on disk");
        assert!(body.contains("package.json"), "{f} carries its evidence citation");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_under_concurrency_interrupts_every_in_flight_turn_without_residue() {
    let (_t, data, ws, pdir) = setup();
    // v1 published serially, so the interrupted v2 has something to preserve.
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("V1 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    runtime_at(&data, good).wiki_build_llm(Some(&ws), false).await.unwrap();
    let v1 = fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap();

    // v2 at concurrency 2: both article turns hang inside the provider.
    let hanging = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        ScriptedTurn::hang_until_cancel(),
        ScriptedTurn::hang_until_cancel(),
    ]));
    let rt2 = runtime_conc(&data, hanging.clone(), 2);
    let build = {
        let rt2 = rt2.clone();
        let ws = ws.clone();
        tokio::spawn(async move { rt2.wiki_build_llm(Some(&ws), false).await })
    };
    let mut spins = 0u32;
    while !wb::is_building(&pdir) && spins < 250_000 {
        tokio::task::yield_now().await;
        spins += 1;
    }
    assert!(wb::is_building(&pdir), "the spawned build must mark itself building");
    // Wait until BOTH article turns are inside the provider call (plan +
    // two = three requests seen). The registration of a turn's session id
    // happens BEFORE its provider call, so at count 3 both ids are set.
    let mut guard = 0u32;
    while hanging.complete_count() < 3 && guard < 20_000 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        guard += 1;
    }
    assert!(hanging.complete_count() >= 3, "plan + BOTH topic turns must be IN FLIGHT: got {}", hanging.complete_count());
    assert_eq!(
        wb::active_turn_sessions(&pdir).len(),
        2,
        "concurrency 2 means TWO model turns alive at once — cancel must reach both"
    );
    assert!(rt2.wiki_build_cancel(Some(&ws)).await, "cancel reaches the live build");

    let err = build.await.expect("task").expect_err("cancelled build must end in error");
    assert!(err.to_string().contains("取消"), "honest cancel error: {err}");
    assert!(
        wb::active_turn_sessions(&pdir).is_empty(),
        "interrupting a CONCURRENT build must still leave NO tracked turn behind"
    );
    let e = wiki::status(&pdir).last_build_error.expect("cancel is recorded with its stage");
    assert_eq!(e.stage, "cancel", "every interrupted turn records the cancel stage: {e:?}");
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap(), v1, "v1 stays readable");
    assert_eq!(wb::load_articles(&pdir).version, 1);

    // The next build starts clean: the drained pool neither poisoned the
    // cancel flag nor left a session for a later cancel to fire at.
    let later = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档（第二版）。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("V2 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let report = runtime_at(&data, later).wiki_build_llm(Some(&ws), false).await.expect("next build must run");
    assert_eq!(report.build, 2);
    assert!(wiki::status(&pdir).last_build_error.is_none(), "a success clears the old failure");
}

// ------------------------------------------------------------ 二.1 coverage API

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coverage_report_reaches_the_desktop_read_only_and_flags_staleness() {
    let (_t, data, ws, pdir) = setup();
    let other = tempfile::tempdir().unwrap();
    let rt = runtime_at(&data, Arc::new(ScriptedProvider::new(vec![])));

    // Never indexed → honestly absent, not a zero-filled lie.
    assert!(rt.wiki_coverage_for(Some(other.path())).is_none());

    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rtf = runtime_at(&data, good);
    rtf.wiki_build_llm(Some(&ws), false).await.unwrap();

    let cov = rt.wiki_coverage_for(Some(&ws)).expect("the build indexed this project");
    assert!(cov.candidate_files >= 4, "the scan counted its candidates: {cov:?}");
    assert!(cov.included_files >= 4, "and what it included: {cov:?}");
    assert!(cov.scanned_at.contains('T'), "timestamped: {}", cov.scanned_at);

    // Fresh publish → nothing stale.
    assert!(wiki::status(&pdir).pages.iter().all(|p| !p.stale), "fresh build reads as current");

    // Source moves, a quick scan re-indexes → the articles standing on it
    // are flagged, others keep their good name. Staleness is derived at read
    // time, so a persisted lie on disk can never survive either.
    touch(&ws, "src/run.ts", "export function launch() {\n  bootV2();\n}\n");
    rtf.wiki_compile(Some(&ws)).unwrap();
    // 快速扫描不得删除已发布模型文章 — bytes, not just registry rows.
    for f in [wiki::AI_OVERVIEW.to_string(), t_quick(), t_flow()] {
        assert!(wiki::compiled_dir(&pdir).join(&f).is_file(), "{f} must survive a quick scan");
    }
    let st = wiki::status(&pdir);
    let stale: Vec<&str> = st.pages.iter().filter(|p| p.stale).map(|p| p.file.as_str()).collect();
    assert!(stale.contains(&t_flow().as_str()), "the article citing the moved file is stale: {stale:?}");
    assert!(!stale.contains(&t_quick().as_str()), "the untouched article is not: {stale:?}");

    // The registry on disk can lie about staleness; effective_pages resets it.
    let mut reg = wiki::load_registry(&pdir);
    if let Some(entry) = reg.pages.iter_mut().find(|p| p.file == t_quick()) {
        entry.stale = true;
    }
    wiki::save_registry(&pdir, &reg).unwrap();
    assert!(
        wiki::effective_pages(&pdir).iter().filter(|p| p.file == t_quick()).all(|p| !p.stale),
        "stale is derived, never trusted from disk"
    );
}

// -------------------------------------------------- 三/五 cross-project isolation

#[test]
fn evidence_jumps_can_never_read_outside_the_bound_project() {
    let (_t, _data, ws, _pdir) = setup();
    // A legitimate window into the bound project works.
    let w = wiki::read_source_window(&ws, "src/run.ts", 1, 2, 400).expect("in-project read");
    assert_eq!((w.start, w.end, w.lines.len()), (1, 2, 2), "window is exactly the ask");

    let sibling = tempfile::tempdir().unwrap();
    let secret = sibling.path().join("secret.txt");
    fs::write(&secret, "另一个项目的私有文件\n").unwrap();

    // Every escape shape must be refused before a byte is read…
    let escapes = ["../secret.txt", "src/../../secret.txt", "..\\secret.txt", "docs/.././../secret.txt"];
    for bad in escapes {
        let err = wiki::read_source_window(&ws, bad, 1, 1, 400)
            .expect_err(&format!("{bad} must never be readable"));
        assert!(err.to_string().contains("不在当前项目内") || err.to_string().contains("非法"), "honest refusal: {err}");
    }
    // …including an absolute path into the sibling project.
    let abs = secret.to_string_lossy().to_string();
    assert!(wiki::read_source_window(&ws, &abs, 1, 1, 400).is_err(), "absolute sibling path refused");
    // Same file, bound as its own project: readable — the refusal is about
    // containment, not a broken reader.
    let ok = wiki::read_source_window(sibling.path(), "secret.txt", 1, 1, 400).expect("own project reads");
    assert_eq!(ok.lines[0], "另一个项目的私有文件");
}

// ------------------------------------------------------ 四 user-edit protection

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_human_edited_article_rides_through_every_later_build_untouched() {
    let (_t, data, ws, pdir) = setup();
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited_src("一次启动从入口函数进入。"),
        cited_manifest("V1 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    runtime_at(&data, good).wiki_build_llm(Some(&ws), false).await.unwrap();

    // The human fixes one article up. Page paths are re-resolved through
    // `compiled_dir` at EVERY touch — a versioned Wiki retires whole
    // directories behind the pointer, so a path captured before a rebuild
    // may name a garbage-collected version, not the living page.
    let human = "人工修订过的内容，模型不许覆盖。\n";
    fs::write(wiki::compiled_dir(&pdir).join(t_quick()), human).unwrap();
    assert!(wb::mark_user_edited(&pdir, &t_quick()));

    // Incremental with nothing stale: zero model turns are even needed.
    let idle = runtime_at(&data, Arc::new(ScriptedProvider::new(vec![])));
    let rep = idle.wiki_build_llm(Some(&ws), true).await.expect("no-op incremental");
    assert!(rep.published.is_empty(), "nothing moved → nothing republished: {rep:?}");

    // A FULL rebuild runs the model again for both slots — and must still
    // leave the human's bytes, the human's record, and no regression in the
    // report: the skip is announced, never silent.
    let later = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("机器重写的快速上手。"),
        cited_src("一次启动从入口函数进入（第二版）。"),
        cited_manifest("V2 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rep2 = runtime_at(&data, later).wiki_build_llm(Some(&ws), false).await.expect("full rebuild");
    assert_eq!(rep2.build, 2);
    assert!(rep2.skipped_user_edited.iter().any(|f| f == &t_quick()), "the skip is reported: {rep2:?}");
    assert_eq!(
        fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap(),
        human,
        "the human text survives the rebuild, riding the carry-forward byte-for-byte"
    );
    // Identity is the TITLE, not the slot: the re-plan of the same topic
    // landed on the same file — and publish turned that collision into a
    // pending conflict copy instead of an overwrite.
    assert!(rep2.pending.iter().any(|f| f == &t_quick()), "conflict copy reported: {rep2:?}");
    let copy = fs::read_to_string(wb::pending_dir(&pdir).join(t_quick())).expect("conflict copy exists");
    assert!(copy.contains("机器重写"), "copy holds the machine's new text: {copy}");
    let reg = wb::load_articles(&pdir);
    let rec = &reg.articles[&t_quick()];
    assert!(rec.user_edited, "and stays marked");
    assert_eq!(rec.build, 1, "its record still names the build the human inherited");
    assert_eq!(reg.articles[&t_flow()].build, 2, "while the untouched slot refreshed normally");
}
