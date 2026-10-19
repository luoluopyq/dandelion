//! G7（任务一 · 结构变化的增量规划）：以真实运行时 + 脚本化模型验证
//! 验收清单里只有全链路才能证明的行为——
//! 完整构建持久化计划基准；新增成组模块走 ONE 次计划增量（不整体重规划）；
//! 改名只重建一次且输入路径重映射（不得新旧双重建）；删除只重建失去证据的
//! 文章；模型 / 设置 / 规则版本各自独立触发完整重规划（但不无谓重建文章）；
//! unknown 或非法 JSON 的计划增量重试一次后回退完整重规划并留下警告；
//! 人工编辑的文章在增量中只落冲突副本；不同 workspace 的构建互不阻塞。

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::wiki_build as wb;
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
    touch(&ws, "src/b2.ts", "export const B = 2;\n");
    // Alphabetically-last source: the 4th code file, past the doc-only
    // grounding (sanitize_inputs prepends the TOP-3 code files — run, a1,
    // b2 — to a docs-only nomination), so the rename/delete/modify
    // scenarios below move a file that only an explicit nomination can
    // put into 快速上手's evidence set.
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
    settings.wiki_build_max_articles = 2;
    tune(&mut settings);
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider)
        .expect("runtime")
}

/// The plan every full/re-plan turn returns: identical to what the first
/// full build recorded, so a re-plan that answers with it must rebuild
/// nothing but the overview.
fn plan_turn() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么把项目跑起来\",\"inputs\":[\"docs/guide.md\"]},\
         {\"title\":\"核心流程\",\"focus\":\"一次典型操作\",\"inputs\":[\"src/zeta_last.ts\"]}]\n```",
    )
}

/// A plan where 核心流程 stands on TWO sources — the deletion scenario needs
/// one to survive so the rebuilt article still has evidence to quote.
fn plan_turn_flow_two_inputs() -> ScriptedTurn {
    ScriptedTurn::text(
        "```json\n[{\"title\":\"快速上手\",\"focus\":\"怎么把项目跑起来\",\"inputs\":[\"docs/guide.md\"]},\
         {\"title\":\"核心流程\",\"focus\":\"一次典型操作\",\"inputs\":[\"src/run.ts\",\"src/zeta_last.ts\"]}]\n```",
    )
}

/// Article text citing `rel` at its full range — inside that article's own
/// evidence pack, so the citation gate passes with zero warnings.
fn citing(rel: &str, lines: u32, body: &str) -> ScriptedTurn {
    ScriptedTurn::text(format!("{body}\n依据 `{rel}:L1-L{lines}`。\n"))
}

fn overview_turn(v: &str) -> ScriptedTurn {
    citing("docs/guide.md", 3, &format!("{v} ## 学习顺序\n1. [快速上手](a.md)"))
}

fn t_quick() -> String {
    wb::topic_file_for("快速上手")
}

fn t_flow() -> String {
    wb::topic_file_for("核心流程")
}

/// One full build on a fresh workspace, script consumed exactly.
async fn full_build(data: &std::path::Path, ws: &std::path::Path, plan: ScriptedTurn) {
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan,
        citing("docs/guide.md", 3, "先读入口文档，按说明安装并启动。"),
        citing("src/zeta_last.ts", 2, "一次启动从入口函数进入。"),
        overview_turn("概览 V1。"),
    ]));
    let rt = runtime_at(data, provider.clone(), |_| {});
    rt.wiki_build_llm(Some(ws), false).await.unwrap();
    assert_eq!(provider.complete_count(), 4);
}

fn compiled(pdir: &std::path::Path, f: &str) -> String {
    fs::read_to_string(wiki::compiled_dir(pdir).join(f)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_build_persists_the_plan_basis_for_future_increments() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        citing("docs/guide.md", 3, "先读入口文档。"),
        citing("src/zeta_last.ts", 2, "一次启动从入口函数进入。"),
        overview_turn("概览 V1。"),
    ]));
    let rt = runtime_at(&data, provider, |s| s.wiki_llm_model = "wiki-pro".into());
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();

    let basis = wb::load_plan_basis(&pdir).expect("full build must persist plan-basis.json");
    assert_eq!(basis.model, "wiki-pro", "the plan's model identity is recorded");
    assert_eq!(basis.rules, wb::PLAN_RULES_VERSION);
    assert!(!basis.settings_key.is_empty());
    for rel in ["package.json", "src/zeta_last.ts", "docs/guide.md"] {
        assert!(basis.files.contains_key(rel), "basis hashes cover {rel}: {:?}", basis.files.keys());
    }
    assert_eq!(basis.topics.len(), 2, "topic identities ride in the basis, overview excluded");
    assert!(basis.topics.iter().all(|t| t.file.starts_with("TOPIC-") && !t.inputs.is_empty()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_module_gets_one_delta_turn_not_a_full_replan() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;
    let quick_before = compiled(&pdir, &t_quick());

    // A coherent new top-level module: two files, both in one fresh module —
    // past the new-file thresholds (2 files / 20% share), never scatter noise.
    touch(&ws, "plugins/auth.ts", "export function login(u: string) {\n  check(u);\n}\n");
    touch(&ws, "plugins/session.ts", "export const TTL = 60;\n");

    // Script: delta answer + new article + overview ONLY. A full-replan path
    // would want a different script and miscount here.
    let delta = ScriptedTurn::text(
        "```json\n{\"新增主题\":[{\"title\":\"插件系统\",\"focus\":\"插件如何注册与鉴权\",\
         \"inputs\":[\"plugins/auth.ts\",\"plugins/session.ts\"]}],\"更新主题\":[],\"不纳入\":[]}\n```",
    );
    let provider = Arc::new(ScriptedProvider::new(vec![
        delta,
        citing("plugins/auth.ts", 2, "插件通过注册表挂载，鉴权在登录入口完成。"),
        overview_turn("概览 V2。"),
    ]));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert!(report.incremental);
    assert_eq!(provider.complete_count(), 3, "one delta voice, no full re-plan");
    let plugin = wb::topic_file_for("插件系统");
    assert_eq!(report.targets, vec![plugin.clone(), AI_OVERVIEW.to_string()]);
    assert!(wiki::compiled_dir(&pdir).join(&plugin).is_file(), "new topic published");

    // Untouched articles keep their build-1 bytes; nothing was pruned.
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.version, 2);
    assert_eq!(reg.articles[&t_quick()].build, 1);
    assert_eq!(compiled(&pdir, &t_quick()), quick_before, "unaffected article bytes untouched");
    assert_eq!(reg.articles.len(), 4, "topics + overview, no casualties");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The basis now records the new module and the placed topic — the next
    // incremental must not ask about these files again.
    let basis = wb::load_plan_basis(&pdir).unwrap();
    assert_eq!(basis.topics.len(), 3);
    assert!(basis.files.contains_key("plugins/auth.ts"));
    let idle = Arc::new(ScriptedProvider::new(vec![]));
    let rt3 = runtime_at(&data, idle.clone(), |_| {});
    let quiet = rt3.wiki_build_llm(Some(&ws), true).await.unwrap();
    assert!(quiet.targets.is_empty() && quiet.warnings.is_empty());
    assert_eq!(idle.complete_count(), 0, "placed module converges — no repeat question");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_rebuilds_the_article_once_with_remapped_inputs() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;

    // zeta_last.ts -> flow.ts: same bytes, moved. Must NOT read as
    // "evidence lost" + "mystery new file" (which would double-build).
    fs::rename(ws.join("src/zeta_last.ts"), ws.join("src/flow.ts")).unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![citing(
        "src/flow.ts",
        2,
        "启动入口现在住在 flow.ts。",
    )]));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(provider.complete_count(), 1, "one article turn, no plan, no delta");
    assert_eq!(report.targets, vec![t_flow()], "no twin topic for the moved file");
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.articles.len(), 3, "rename must not mint a second article");
    let inputs: Vec<&String> = reg.articles[&t_flow()].inputs.keys().collect();
    assert!(inputs.contains(&&"src/flow.ts".to_string()), "inputs remap to the new path: {inputs:?}");
    assert!(!inputs.contains(&&"src/zeta_last.ts".to_string()), "old path must not linger: {inputs:?}");
    assert!(compiled(&pdir, &t_flow()).contains("flow.ts"));

    // Converged: a second incremental asks the model nothing at all.
    let idle = Arc::new(ScriptedProvider::new(vec![]));
    let rt3 = runtime_at(&data, idle.clone(), |_| {});
    let quiet = rt3.wiki_build_llm(Some(&ws), true).await.unwrap();
    assert!(quiet.targets.is_empty(), "{:?}", quiet.targets);
    assert_eq!(idle.complete_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deletion_rebuilds_only_the_article_that_lost_evidence() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn_flow_two_inputs(),
        citing("docs/guide.md", 3, "先读入口文档。"),
        citing("src/run.ts", 2, "启动链从 run 进入 boot。"),
        overview_turn("概览 V1。"),
    ]));
    let rt = runtime_at(&data, provider, |_| {});
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    let quick_before = compiled(&pdir, &t_quick());

    fs::remove_file(ws.join("src/zeta_last.ts")).unwrap();

    // 核心流程 still stands on src/run.ts; 快速上手 never cited the file.
    let p2 = Arc::new(ScriptedProvider::new(vec![citing("src/run.ts", 2, "重建后：启动链见 run 入口。")]));
    let rt2 = runtime_at(&data, p2.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(p2.complete_count(), 1, "only the article that lost evidence is rebuilt");
    assert_eq!(report.targets, vec![t_flow()]);
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.articles[&t_flow()].build, 2);
    assert_eq!(reg.articles[&t_quick()].build, 1, "unrelated article untouched");
    assert_eq!(compiled(&pdir, &t_quick()), quick_before);
    assert_eq!(reg.articles[AI_OVERVIEW].build, 1, "overview untouched while its own evidence held");
    let inputs: Vec<&String> = reg.articles[&t_flow()].inputs.keys().collect();
    assert!(inputs.contains(&&"src/run.ts".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_change_replans_the_plan_without_rebuilding_articles() {
    let (_t, data, ws, pdir) = setup();
    let provider = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        citing("docs/guide.md", 3, "先读入口文档。"),
        citing("src/zeta_last.ts", 2, "一次启动从入口函数进入。"),
        overview_turn("概览 V1。"),
    ]));
    let rt = runtime_at(&data, provider, |s| s.wiki_llm_model = "wiki-pro".into());
    rt.wiki_build_llm(Some(&ws), false).await.unwrap();
    let quick_before = compiled(&pdir, &t_quick());
    let flow_before = compiled(&pdir, &t_flow());

    // Different planning model, workspace unchanged: the PLAN is re-asked,
    // articles stand on unchanged evidence and must not move — and since
    // the plan came back with the same topics, even the overview keeps its
    // bytes: nothing was affected, nothing rebuilds.
    let p2 = Arc::new(ScriptedProvider::new(vec![plan_turn()]));
    let rt2 = runtime_at(&data, p2.clone(), |s| s.wiki_llm_model = "wiki-pro-max".into());
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(p2.complete_count(), 1, "the plan alone is re-asked");
    assert!(p2.history().iter().all(|h| h.model == "wiki-pro-max"));
    assert!(report.targets.is_empty(), "re-plan without casualties: {:?}", report.targets);
    assert_eq!(compiled(&pdir, &t_quick()), quick_before, "unchanged evidence must not rebuild");
    assert_eq!(compiled(&pdir, &t_flow()), flow_before);
    let basis = wb::load_plan_basis(&pdir).unwrap();
    assert_eq!(basis.model, "wiki-pro-max", "re-plan re-anchors on the new model once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_shaping_settings_change_triggers_a_replan() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;

    // max_articles shapes the plan itself (fingerprint covers it), unlike
    // the detection thresholds (unit-tested separately as fingerprint-quiet).
    // Raising the budget to 3 re-plans, and the coverage floor then legally
    // grows the wiki by one required topic (仓库导览) — but the two original
    // articles keep their bytes.
    let tour = wb::topic_file_for("仓库导览");
    let p2 = Arc::new(ScriptedProvider::new(vec![
        plan_turn(),
        citing("docs/guide.md", 3, "按目录结构带你走一遍仓库。"),
        overview_turn("概览 V2。"),
    ]));
    let rt2 = runtime_at(&data, p2.clone(), |s| s.wiki_build_max_articles = 3);
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(p2.complete_count(), 3, "re-plan + the one new topic + overview");
    assert_eq!(report.targets, vec![tour.clone(), AI_OVERVIEW.to_string()]);
    let reg = wb::load_articles(&pdir);
    assert_eq!(reg.articles[&t_flow()].build, 1, "unchanged articles keep their bytes");
    assert_eq!(reg.articles[&t_quick()].build, 1);
    assert!(reg.articles.contains_key(&tour), "budget growth fills the required-topic floor");
    assert_eq!(wb::load_plan_basis(&pdir).unwrap().settings_key, {
        use dandelion_core::config::Settings;
        let mut s = Settings::defaults_configured();
        s.wiki_build_max_articles = 3;
        wb::plan_settings_fingerprint(&s)
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_version_change_triggers_a_replan() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;

    // Simulate the shipped upgrade: a basis written under an older planning
    // rules version invalidates exactly once.
    let mut basis = wb::load_plan_basis(&pdir).unwrap();
    basis.rules = "plan-v0-legacy".into();
    wb::save_plan_basis(&pdir, &basis).unwrap();

    let p2 = Arc::new(ScriptedProvider::new(vec![plan_turn()]));
    let rt2 = runtime_at(&data, p2.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(p2.complete_count(), 1, "one re-plan, zero rebuilds");
    assert!(report.targets.is_empty(), "{:?}", report.targets);
    assert_eq!(wb::load_plan_basis(&pdir).unwrap().rules, wb::PLAN_RULES_VERSION, "re-anchored once");
    // Third run: basis is current again — silent.
    let idle = Arc::new(ScriptedProvider::new(vec![]));
    let rt3 = runtime_at(&data, idle.clone(), |_| {});
    rt3.wiki_build_llm(Some(&ws), true).await.unwrap();
    assert_eq!(idle.complete_count(), 0);
}

/// Add a coherent module and expect the delta turn to fail twice then fall
/// back — shared by the unknown / invalid-JSON fallback scenarios.
fn add_plugin_module(ws: &std::path::Path) {
    touch(ws, "plugins/auth.ts", "export function login(u: string) {\n  check(u);\n}\n");
    touch(ws, "plugins/session.ts", "export const TTL = 60;\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_delta_retries_once_then_falls_back_to_full_replan() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;
    add_plugin_module(&ws);

    // Honest "unknown" both attempts (wiki_build_plan_retry_max = 1 ⇒ 2
    // tries): the build must fall back to a FULL re-plan, loudly. The
    // re-plan returns the existing topics, so no article moves.
    let unknown = || ScriptedTurn::text(r#"{"unknown":true}"#);
    let provider = Arc::new(ScriptedProvider::new(vec![unknown(), unknown(), plan_turn()]));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(provider.complete_count(), 3, "2 delta attempts + re-plan");
    assert!(
        report.warnings.iter().any(|w| w.contains("增量计划不可用") && w.contains("unknown") && w.contains("完整重规划")),
        "fallback must be surfaced verbatim: {:?}",
        report.warnings
    );
    // The fallback re-plan returned the existing topics: nothing new placed,
    // nothing lost, nothing rebuilt.
    assert_eq!(wb::load_articles(&pdir).articles.len(), 3);
    assert!(report.targets.is_empty(), "{:?}", report.targets);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_delta_json_retries_once_then_falls_back_to_full_replan() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;
    add_plugin_module(&ws);

    let babble = || ScriptedTurn::text("对不起，这一段我没法按格式输出。");
    let provider = Arc::new(ScriptedProvider::new(vec![babble(), babble(), plan_turn()]));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(provider.complete_count(), 3, "one retry on unparsable output, then re-plan");
    assert!(
        report.warnings.iter().any(|w| w.contains("增量计划不可用") && w.contains("完整重规划")),
        "{:?}",
        report.warnings
    );
    assert_eq!(wb::load_articles(&pdir).articles.len(), 3);
    assert!(!wb::load_articles(&pdir).articles.contains_key(&wb::topic_file_for("插件系统")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_edited_article_becomes_a_pending_conflict_not_an_overwrite() {
    let (_t, data, ws, pdir) = setup();
    full_build(&data, &ws, plan_turn()).await;
    assert!(wb::mark_user_edited(&pdir, &t_flow()), "the edit flag must stick to the record");
    let human_text = compiled(&pdir, &t_flow());

    touch(&ws, "src/zeta_last.ts", "export function boot() {\n  runV2();\n}\n");

    let provider = Arc::new(ScriptedProvider::new(vec![citing("src/zeta_last.ts", 2, "机器重写的新内容。")]));
    let rt2 = runtime_at(&data, provider.clone(), |_| {});
    let report = rt2.wiki_build_llm(Some(&ws), true).await.unwrap();

    assert_eq!(report.skipped_user_edited, vec![t_flow()], "the skip is reported by file");
    assert_eq!(compiled(&pdir, &t_flow()), human_text, "human text never moves without consent");
    assert!(wb::load_articles(&pdir).articles[&t_flow()].user_edited, "protection survives the build");
    assert!(report.published.iter().all(|f| f != &t_flow()));
    assert_eq!(provider.complete_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn different_workspaces_build_in_parallel_without_interference() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let mk = |name: &str| {
        let ws = tmp.path().join(name);
        touch(&ws, "package.json", "{\"name\":\"app\",\"main\":\"src/run.ts\"}");
        touch(&ws, "src/run.ts", "export function launch() {\n  boot();\n}\n");
        touch(&ws, "src/zeta_last.ts", "export function boot() {\n  run();\n}\n");
        touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
        ws
    };
    let ws_a = mk("ws-a");
    let ws_b = mk("ws-b");
    let script = || {
        vec![
            plan_turn(),
            citing("docs/guide.md", 3, "先读入口文档。"),
            citing("src/zeta_last.ts", 2, "一次启动从入口函数进入。"),
            overview_turn("概览 V1。"),
        ]
    };
    let pa = Arc::new(ScriptedProvider::new(script()));
    let pb = Arc::new(ScriptedProvider::new(script()));
    let ra = runtime_at(&data, pa.clone(), |_| {});
    let rb = runtime_at(&data, pb.clone(), |_| {});

    // Same data root, different projects: the per-project build lock must
    // not make one workspace wait on (or corrupt) the other.
    let (a, b) = tokio::join!(ra.wiki_build_llm(Some(&ws_a), false), rb.wiki_build_llm(Some(&ws_b), false));
    assert_eq!(a.expect("ws-a builds").build, 1);
    assert_eq!(b.expect("ws-b builds").build, 1);

    let pdir_a = resolved(&data, &ws_a);
    let pdir_b = resolved(&data, &ws_b);
    assert_ne!(pdir_a, pdir_b, "workspaces resolve to separate space homes");
    assert_eq!(wb::load_articles(&pdir_a).version, 1);
    assert_eq!(wb::load_articles(&pdir_b).version, 1);
    assert!(wb::load_plan_basis(&pdir_a).is_some() && wb::load_plan_basis(&pdir_b).is_some());
    assert_eq!(pa.complete_count(), 4);
    assert_eq!(pb.complete_count(), 4);
}
