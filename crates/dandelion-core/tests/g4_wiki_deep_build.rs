//! G4: the deep-build orchestration (Wiki phase S3) driven through the real
//! runtime with a scripted model. Covers the spec items that only the full
//! loop can prove: a successful build publishes plan → topic articles →
//! overview in that order and consumes exactly one model turn per step; a
//! provider failure mid-build leaves the previous Wiki byte-identical and
//! names the failed stage in status; an article without valid citations gets
//! exactly one honest retry quoting the failure, then the whole build fails
//! with zero writes; an incremental rebuild skips the plan and regenerates
//! only what staleness names.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ChatMessage;
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

/// `(data root, workspace, this project's resolved Wiki directory)` — the
/// runtime resolves `project_data_dir` from the host data root, so every
/// assertion must read the resolved directory too.
fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{\"name\":\"app\",\"main\":\"src/run.ts\"}");
    touch(&ws, "src/run.ts", "export function launch() {\n  boot();\n}\n");
    touch(&ws, "src/a1.ts", "export const A = 1;\n");
    touch(&ws, "src/b2.ts", "export const B = 2;\n");
    // Alphabetically-last source: the 4th code file, past both the pack
    // floor (top-3 index files) and the doc-only grounding (top-3 code
    // files), so it only lands in a pack that names it explicitly.
    touch(&ws, "src/zeta_last.ts", "export function boot() {\n  run();\n}\n");
    touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
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
    // Two articles keep the scripted turn count small while still exercising
    // plan → topics → overview ordering.
    settings.wiki_build_max_articles = 2;
    tune(&mut settings);
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

/// A plan naming exactly the two budget slots, both required topics.
fn plan_turn() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么把项目跑起来\",\"inputs\":[\"docs/guide.md\"]},\
         {\"title\":\"核心流程\",\"focus\":\"一次典型操作\",\"inputs\":[\"src/zeta_last.ts\"]}]\n```",
    )
}

fn cited(body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `docs/guide.md:L1-L3`。\n"))
}

/// Article files are title-stable (`TOPIC-<hash(title)>`), so expectations
/// compute them from the plan's titles rather than any positional number.
fn t_quick() -> String {
    wb::topic_file_for("快速上手")
}

fn t_flow() -> String {
    wb::topic_file_for("核心流程")
}

fn user_text(provider: &ScriptedProvider) -> String {
    provider
        .last_messages()
        .iter()
        .map(|m| match m {
            ChatMessage::User { content, .. } => content.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn empty_workspace_stops_before_any_model_call() {
    let (_t, data, ws, pdir) = setup();
    fs::remove_dir_all(&ws).unwrap();
    fs::create_dir_all(&ws).unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let err = rt.wiki_build_llm(Some(&ws), false).await.unwrap_err();
    assert!(err.to_string().contains("尚未调用模型"), "{err}");
    assert_eq!(provider.complete_count(), 0);
    assert_eq!(wiki::status(&pdir).last_build_error.unwrap().stage, "sources");
    assert_eq!(wb::load_articles(&pdir).version, 0);
    assert!(!wiki::compiled_dir(&pdir).join(AI_OVERVIEW).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_sources_preserve_last_published_articles_without_model_calls() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(), cited("入门说明。"), cited("流程说明。"), cited("项目概览。"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    let previous = fs::read(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    let calls = provider.complete_count();
    fs::remove_dir_all(&ws).unwrap();
    fs::create_dir_all(&ws).unwrap();
    for incremental in [false, true] {
        let err = rt.wiki_build_llm(Some(&ws), incremental).await.unwrap_err();
        assert!(err.to_string().contains("尚未调用模型"), "{err}");
        assert_eq!(provider.complete_count(), calls);
        assert_eq!(fs::read(wiki::compiled_dir(&pdir).join(t_quick())).unwrap(), previous);
        assert_eq!(wb::load_articles(&pdir).version, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_build_plans_then_articles_then_overview_with_one_turn_each() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档，按说明安装并启动。"),
        cited("一次启动从入口函数进入。"),
        cited("这是一个示例仓库。## 学习顺序\n1. [快速上手](TOPIC-1.md)\n2. [核心流程](TOPIC-2.md)"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |s| s.wiki_llm_model = "wiki-pro".into());

    let report = rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    assert_eq!(report.build, 1);
    assert!(!report.incremental);
    assert_eq!(
        report.targets,
        vec![t_quick(), t_flow(), AI_OVERVIEW.to_string()],
        "plan slots first, overview last"
    );
    assert_eq!(report.published.len(), 3);

    // Exactly one model turn per step — no hidden extra calls.
    assert_eq!(provider.complete_count(), 4, "plan + 2 articles + overview");
    // The wiki_llm_model knob reached every request.
    assert!(
        provider.history().iter().all(|h| h.model == "wiki-pro"),
        "every deep-build turn must honour dandelion.wiki.llmModel"
    );
    // The overview prompt carried a real evidence pack with line numbers.
    let last = user_text(&provider);
    assert!(last.contains("### 文件 "), "overview prompt must embed the evidence pack: {last}");
    assert!(last.contains("1: "), "evidence lines carry their real numbers");
    assert!(last.contains("学习顺序"), "overview prompt must demand a learning order");

    // Published state: registry, files, roles, no error.
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.version, 1);
    assert!(reg.articles[&t_quick()].inputs.contains_key("docs/guide.md"));
    for f in [t_quick(), t_flow(), AI_OVERVIEW.to_string()] {
        assert!(wiki::compiled_dir(&pdir).join(&f).is_file(), "{f} must be published");
    }
    let pages = wiki::effective_pages(&pdir);
    assert!(pages.iter().any(|p| p.file == t_quick() && p.role.as_deref() == Some("model")));
    assert!(pages.iter().any(|p| p.file == AI_OVERVIEW && p.role.as_deref() == Some("overview")));
    assert!(wiki::status(&pdir).last_build_error.is_none());
    assert!(wb::stale_article_files(&pdir, &wi::build_index(&pdir, &ws, 100, 65_536).unwrap()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_failure_mid_build_keeps_previous_wiki_and_names_the_stage() {
    let (_t, data, ws, pdir) = setup();
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited("一次启动从入口函数进入。"),
        cited("V1 概览。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, good, |_| {});
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    let before = fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap();

    // Second build: the plan succeeds, the first article turn dies.
    let bad = Arc::new(ScriptedProvider::new(vec![plan_turn(), ScriptedTurn::overflow()]));
    let rt2 = runtime_at(&data, bad.clone(), |_| {});
    let err = rt2.wiki_build_llm(Some(&ws), false).await.unwrap_err();
    assert!(err.to_string().contains("overflow"), "the provider error surfaces: {err}");
    assert_eq!(bad.complete_count(), 2, "failure stops the build immediately");

    // Old Wiki untouched, failure visible with its stage.
    assert_eq!(fs::read(wiki::compiled_dir(&pdir).join(AI_OVERVIEW)).unwrap(), before);
    assert_eq!(wb::load_articles(&pdir).version, 1);
    let st = wiki::status(&pdir);
    let e = st.last_build_error.expect("failure recorded");
    assert_eq!(e.stage, "article");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfounded_article_gets_one_retry_then_fails_the_whole_build() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        ScriptedTurn::text("这是一段没有任何出处的断言，不该发布。"),
        ScriptedTurn::text("还是没有出处。"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let err = rt.wiki_build_llm(Some(&ws), false).await.unwrap_err();
    assert!(err.to_string().contains("《快速上手》"), "the failing article is named: {err}");

    // Exactly one retry, and it quotes the verification failure.
    assert_eq!(provider.complete_count(), 3, "plan + article + one retry, then stop");
    let retry = user_text(&provider);
    assert!(retry.contains("没有通过校验"), "the retry must tell the model why it failed: {retry}");

    // Zero writes: nothing published, registry still empty, stage visible.
    assert!(!wiki::compiled_dir(&pdir).join(t_quick()).exists());
    assert!(!wiki::compiled_dir(&pdir).join(AI_OVERVIEW).exists());
    assert_eq!(wb::load_articles(&pdir).version, 0);
    assert_eq!(wiki::status(&pdir).last_build_error.expect("error").stage, "verify");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fabricated_line_number_is_neutralized_not_fatal_and_the_build_survives() {
    let (_t, data, ws, pdir) = setup();
    // TOPIC-1 keeps hallucinating a past-EOF line even after the retry
    // quotes the exact failure (real models do this). A past-EOF claim is
    // not fetchable, so re-evidence has nothing to read and the sentence
    // must be DEMOTED — 【待核实】 on the claim, the fabricated token gone —
    // instead of killing the whole build, while the valid cite's sentence
    // stands as fact. The demotion surfaces as a report warning AND in the
    // per-article conclusion count.
    let bad = |n: u32| {
        ScriptedTurn::text(format!(
            "第{n}次尝试：正常结论见 `docs/guide.md:L1-L3`；编造的见 `docs/guide.md:L99{n}`。"
        ))
    };
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        bad(1),
        bad(2),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("build survives");
    assert_eq!(provider.complete_count(), 5, "no third model turn for a rescuable article");
    assert!(report.published.iter().any(|f| f == &t_quick()), "{:?}", report.published);
    assert_eq!(report.refetch_rounds, 0, "past-EOF claims cannot be re-read");
    assert_eq!(report.conclusion_demotions, 1, "exactly the fabricated sentence: {:?}", report.warnings);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("快速上手"), "{}", report.warnings[0]);
    assert!(report.warnings[0].contains("待核实"), "{}", report.warnings[0]);
    let top = fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    assert!(top.contains("`docs/guide.md:L1-L3`"), "valid cite untouched: {top}");
    assert!(!top.contains("L991") && !top.contains("L992"), "fabricated lines never publish: {top}");
    assert!(top.contains("【待核实】"), "the claim itself is demoted, not just its marker: {top}");
    assert!(top.contains("（引用待核实：docs/guide.md）"), "repair is visible: {top}");
    assert!(wiki::status(&pdir).last_build_error.is_none(), "a repaired build is a success");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_rebuild_skips_the_plan_and_rewrites_only_stale_articles() {
    let (_t, data, ws, pdir) = setup();
    let good = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        cited("先读入口文档。"),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, good, |_| {});
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();

    // Only the deep article's own input moves: TOPIC-1 (priority floor files
    // + guide) and the overview must not be dragged along.
    touch(&ws, "src/zeta_last.ts", "export function boot() {\n  runV2();\n}\n");
    let idx = wi::build_index(&pdir, &ws, 100, 65_536).unwrap();
    let stale = wb::stale_article_files(&pdir, &idx);
    assert_eq!(stale, vec![t_flow()], "staleness names exactly the article standing on the moved file");

    // Script exactly `stale.len()` turns: any extra target or hidden plan
    // turn would exhaust the script and fail the build.
    let turns: Vec<ScriptedTurn> = (0..stale.len()).map(|_| cited("重建后的内容。")).collect();
    let provider = Arc::new(ScriptedProvider::new(turns));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert!(report.incremental);
    assert_eq!(report.build, 2);
    let mut want = stale.clone();
    let mut got = report.targets.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want, "only the stale set is regenerated");
    assert_eq!(provider.complete_count(), stale.len(), "incremental never re-plans");

    // Rebuilt articles carry build 2; untouched ones keep build 1 — the
    // overview is only rebuilt when staleness names it.
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.articles[&t_quick()].build, 1);
    assert_eq!(reg.articles[&t_flow()].build, 2);
    assert_eq!(reg.articles[AI_OVERVIEW].build, 1, "overview rides along only when its own evidence moved");
    let idx2 = wi::build_index(&pdir, &ws, 100, 65_536).unwrap();
    assert!(wb::stale_article_files(&pdir, &idx2).is_empty(), "rebuild clears staleness");

    // With nothing stale, an incremental build burns zero model turns.
    let idle = Arc::new(ScriptedProvider::new(vec![]));
    let rt3 = runtime_at(&data, idle.clone(), |_| {});
    let quiet = rt3.wiki_build_llm(Some(&ws), true).await.unwrap();
    assert_eq!(quiet.build, 2);
    assert!(quiet.targets.is_empty() && quiet.published.is_empty());
    assert_eq!(idle.complete_count(), 0, "no-op incremental must not call the model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_length_cut_turn_is_spliced_and_not_published_mid_sentence() {
    let (_t, data, ws, pdir) = setup();
    // The provider dies mid-citation with `finish_reason = "length"` — the
    // exact shape seen live at acc04, where the real-model OVERVIEW ended
    // at 「例如 Gh」. A truncated body still VERIFIES (every citation
    // inside a cut-off article passes; the cut itself carries no `:L`
    // token), so the only defense is the loop's own truncation
    // continuation — one "continue exactly from where you stopped" half
    // spliced onto the first. `wiki_model_turn`'s max_iterations=2 exists
    // precisely to keep that continuation reachable (Ask mode carries an
    // empty tool registry, so the extra iteration can never be a tool
    // round). With iterations pinned to 1 the cut half publishes as-is and
    // this test fails.
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        ScriptedTurn::truncated("先读入口文档，按说明安装并启动。依据 `docs/gu"),
        ScriptedTurn::text("ide.md:L1-L3`。"),
        cited("一次启动从入口函数进入。"),
        cited("概览 V1。## 学习顺序\n1. [快速上手](TOPIC-1.md)"),
    ]));
    let rt = runtime_at(&data, provider.clone(), |_| {});
    let report = rt.wiki_build_llm(Some(&ws), false).await.expect("spliced build succeeds");

    // Exactly one continuation for one cut: plan + half + splice + article2
    // + overview. No hidden retries beyond the splice.
    assert_eq!(provider.complete_count(), 5, "cut + one splice, nothing else: {:?}", report.targets);
    let top = fs::read_to_string(wiki::compiled_dir(&pdir).join(t_quick())).unwrap();
    assert!(
        top.contains("先读入口文档，按说明安装并启动。依据 `docs/guide.md:L1-L3`。"),
        "cut and continuation publish as ONE seamless body: {top}"
    );
    assert!(!top.contains("待核实"), "a spliced body needs no demotion: {top}");
    assert!(report.warnings.is_empty(), "splice is silent: {:?}", report.warnings);
    assert!(wiki::status(&pdir).last_build_error.is_none());
}
