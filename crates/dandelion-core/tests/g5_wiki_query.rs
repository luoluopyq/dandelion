//! G5: the unified query surface (Wiki phase S4). One core function —
//! `wiki::gather` — answers every reader: the native QA turn and external
//! agent engines through the wiki bridge. Covers the spec items: QA searches
//! the raw evidence index FIRST and Wiki pages alongside; every raw hit is
//! citable as 文件:行号; an evidence-free question says so honestly and costs
//! no model call; external agents get the SAME query (identical rendering,
//! identical order) and can only ever see the project their grant is bound
//! to.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::ChatMessage;
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use dandelion_core::wiki::{self, AI_OVERVIEW};
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use serde_json::json;
use std::fs;
use std::sync::Arc;

/// The bridge is ONE process-global loopback server living on whichever
/// tokio runtime first started it; test runtimes are private, so bridge
/// tests serialize (each re-ensures — `ensure_server` rebinds after the
/// liveness check). Same discipline as the onboarding suite.
static BRIDGE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bridge_lock() -> std::sync::MutexGuard<'static, ()> {
    BRIDGE_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

/// Fixture every test shares: a source file carrying a unique symbol the
/// raw content pass will find, plus a doc and a manifest.
fn touch_fixture(ws: &std::path::Path) {
    touch(ws, "package.json", "{\"name\":\"app\",\"main\":\"src/engine.ts\"}");
    touch(ws, "src/engine.ts", "export function launchReactor() {\n  ignite();\n}\n");
    touch(ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
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

async fn rpc(url: &str, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = json!({"jsonrpc": "2.0", "id": "1", "method": method, "params": params});
    let res = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("rpc send");
    res.json().await.expect("rpc json")
}

/// Ask the runtime to (re)ensure the bridge and read host:port back through
/// a probe token's URL (the token map embeds the live port).
async fn bridge_host(rt: &Runtime) -> String {
    let token = rt
        .wiki_bridge_grant("probe-session", std::path::Path::new("C:/"), PermissionMode::Ask)
        .await
        .unwrap();
    let url = dandelion_core::wiki_bridge::bridge_url(&token).expect("bridge url");
    let rest = url.strip_prefix("http://").unwrap_or(&url);
    let host = rest.split('/').next().unwrap_or(rest).to_string();
    rt.revoke_wiki_grants("probe-session");
    host
}

/// The space-resolved Wiki home — the same resolution the runtime uses
/// (post phase-2 migration a registered workspace lives under
/// `wiki/spaces/<id>`, so assertions must read the resolved dir too).
fn resolved(data: &std::path::Path, ws: &std::path::Path) -> std::path::PathBuf {
    let store = dandelion_core::store::sqlite::Store::open(data, 5000).unwrap();
    dandelion_core::wiki_space_migrate::ensure_space_dir(&store, data, ws, true).unwrap()
}

fn runtime_at(ws: &std::path::Path, data: &std::path::Path, provider: Arc<ScriptedProvider>) -> Runtime {
    let mut settings = Settings::defaults_configured();
    settings.permission_mode = PermissionMode::Auto;
    settings.workspace = Some(ws.to_path_buf());
    Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.to_path_buf())), settings, provider).expect("runtime")
}

#[test]
fn gather_puts_raw_files_first_keeps_pages_and_is_honest_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch_fixture(&ws);
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    assert!(wi::load_index(&data).is_some(), "a quick compile keeps the evidence index current");

    // A published model article that also speaks about the same symbol, so
    // both evidence kinds can hit the same question.
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx,
        &[wb::StagedArticle {
            file: "TOPIC-engine.md".into(),
            title: "引擎".into(),
            focus: String::new(),
            content: "`launchReactor` 的引导流程见 `src/engine.ts:L1-L3`。\n".into(),
            inputs: vec!["src/engine.ts".into()],
            ranges: Vec::new(),
        }],
    )
    .unwrap();

    let hits = wiki::gather(&data, &ws, "launchReactor 引导", 4, 100, 65_536);
    assert!(!hits.is_empty(), "both evidence kinds speak about the symbol");
    assert_eq!(hits[0].kind.as_deref(), Some("raw"), "raw evidence ranks first");
    assert_eq!(hits[0].page, "src/engine.ts");
    assert_eq!(hits[0].start, Some(1));
    // The window spans the whole function body (lines 1..=3), not just the
    // declaration line: answers need the implementation, not a one-liner.
    assert_eq!(hits[0].end, Some(3));
    assert!(
        hits.iter().any(|h| h.kind.as_deref() == Some("wiki")),
        "Wiki prose keeps a slot when both sides hit: {hits:?}"
    );
    let label = wiki::hit_label(&hits[0]);
    assert!(label.contains("src/engine.ts:L1") && label.contains("原始文件"), "{label}");

    // Nothing to ground an answer in → empty, and the caller must say so.
    assert!(wiki::gather(&data, &ws, "zxqwkvjm nonexistent token", 4, 100, 65_536).is_empty());
    // A single slot still goes to the preferred source.
    let one = wiki::gather(&data, &ws, "launchReactor 引导", 1, 100, 65_536);
    assert_eq!(one.first().and_then(|h| h.kind.as_deref()), Some("raw"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_qa_turn_reads_raw_files_first_and_refuses_without_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch_fixture(&ws);
    fs::create_dir_all(&data).unwrap();
    let pdir = resolved(&data, &ws);
    wiki::compile(&pdir, &ws, 100, 65_536).unwrap();

    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(
        "依据 [1]，`launchReactor` 在 src/engine.ts 中启动反应堆。",
    )]));
    let rt = runtime_at(&ws, &data, provider.clone());

    let ans = rt.wiki_ask_for(Some(&ws), "launchReactor 是怎么启动的").await.unwrap();
    assert_eq!(provider.complete_count(), 1, "one grounded turn");
    let prompt = user_text(&provider);
    assert!(prompt.contains("原始文件，优先采信"), "the prompt labels raw evidence: {prompt}");
    assert!(prompt.contains("src/engine.ts:L"), "raw sources are cited with line numbers: {prompt}");
    assert!(prompt.contains("项目 Wiki 文章"), "the prompt names both source kinds");
    assert_eq!(ans.sources.first().and_then(|s| s.kind.as_deref()), Some("raw"));
    assert!(ans.answer.contains("launchReactor"));

    // No evidence anywhere → honest refusal WITHOUT spending a model turn.
    let none = rt.wiki_ask_for(Some(&ws), "赤橙黄绿青蓝紫咒语是什么").await.unwrap();
    assert!(none.sources.is_empty());
    assert!(none.answer.contains("没有找到足够依据"), "{}", none.answer);
    assert_eq!(provider.complete_count(), 1, "an ungrounded question must not call the model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_agents_get_the_same_query_and_never_another_projects_files() {
    let _serial = bridge_lock();
    let tmp = tempfile::tempdir().unwrap();
    let (ws_a, ws_b, data) = (tmp.path().join("A"), tmp.path().join("B"), tmp.path().join("data"));
    touch(&ws_a, "package.json", "{\"name\":\"alpha\"}");
    touch(&ws_a, "src/alpha_engine.ts", "export const ALPHA_MARKER_QUANTUM = 42;\n");
    touch(&ws_b, "package.json", "{\"name\":\"beta\"}");
    touch(&ws_b, "src/beta.ts", "export const BETA_MARKER_PHOENIX = 7;\n");
    fs::create_dir_all(&data).unwrap();
    let pdir_a = resolved(&data, &ws_a);
    let pdir_b = resolved(&data, &ws_b);
    wiki::compile(&pdir_a, &ws_a, 100, 65_536).unwrap();
    wiki::compile(&pdir_b, &ws_b, 100, 65_536).unwrap();

    // Project A: a normal conversation mints the bridge grant the ordinary way.
    let rt = runtime_at(&ws_a, &data, Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")])));
    let sid = rt.new_session(None);
    rt.prompt(PromptRequest::new(sid.clone(), "你好")).await.expect("prompt");
    let token = rt.wiki_bridge_grant(&sid.to_string(), &ws_a, PermissionMode::Auto).await.unwrap();
    let url = format!("http://{}/mcp/{token}", bridge_host(&rt).await);

    // External engines see BOTH tools — query is not an afterthought.
    let list = rpc(&url, "tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"dandelion_wiki_propose"), "{names:?}");
    assert!(names.contains(&"dandelion_wiki_query"), "query capability shipped to adapters: {names:?}");

    // The query answers from A's raw files with clickable provenance.
    let call = rpc(
        &url,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "ALPHA_MARKER_QUANTUM"}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], false, "{call}");
    let text = call["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(text.contains("src/alpha_engine.ts"), "raw hit names the file: {text}");
    assert!(text.contains("ALPHA_MARKER_QUANTUM"), "excerpt carries the matched line: {text}");
    assert!(text.contains(":L"), "provenance is 文件:行号: {text}");
    assert!(!text.contains("BETA_MARKER_PHOENIX"), "never leaks the other project: {text}");

    // 多 Agent 同能力: the bridge reply renders exactly what a native
    // `wiki::gather` produces for the same project and question — same
    // hits, same order, same labels. Not a weaker copy.
    let settings = rt.settings();
    let native = wiki::gather(&pdir_a, &ws_a, "ALPHA_MARKER_QUANTUM", 8, settings.wiki_max_scan_files, settings.wiki_max_file_bytes);
    assert!(!native.is_empty(), "native reader finds the same evidence");
    assert!(text.contains(&wiki::hit_label(&native[0])), "identical first hit and label:\n{text}\nvs\n{}", native[0].page);

    // Project isolation: a B-bound session asks for A's marker and honestly
    // gets "not enough evidence" — not A's file.
    let rt_b = runtime_at(&ws_b, &data, Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("hi")])));
    let sid_b = rt_b.new_session(None);
    rt_b.prompt(PromptRequest::new(sid_b.clone(), "你好")).await.expect("prompt");
    let token_b = rt_b.wiki_bridge_grant(&sid_b.to_string(), &ws_b, PermissionMode::Auto).await.unwrap();
    let url_b = format!("http://{}/mcp/{token_b}", bridge_host(&rt_b).await);
    let foreign = rpc(
        &url_b,
        "tools/call",
        json!({"name": "dandelion_wiki_query", "arguments": {"question": "ALPHA_MARKER_QUANTUM"}}),
    )
    .await;
    let ftext = foreign["result"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(ftext.contains("未命中"), "cross-project question must miss honestly: {ftext}");
    assert!(!ftext.contains("alpha_engine"), "no leak through the miss message: {ftext}");

    // A missing question argument is a usage error, not a crash.
    let bad = rpc(&url, "tools/call", json!({"name": "dandelion_wiki_query", "arguments": {}})).await;
    assert_eq!(bad["result"]["isError"], true, "{bad}");

    rt.revoke_wiki_grants(&sid.to_string());
    rt_b.revoke_wiki_grants(&sid_b.to_string());
    let _ = AI_OVERVIEW;
}

/// Real regression (REA acceptance): section-level heading matching used to
/// be gated to the overview alone, so a deep-build article whose answer sits
/// under its own `##` heading was invisible to QA — a question the article
/// answers verbatim still came back 「不知道」, because the article's best
/// term hits were in its HEADING (skipped by the line pass) and the
/// overview's +4-weighted sections owned the slots. The question below is
/// shaped exactly like 「从哪里开始读代码」 against a tour article.
#[test]
fn model_article_sections_reach_qa_evidence_not_only_the_overview() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch_fixture(&ws);
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx,
        &[
            wb::StagedArticle {
                focus: String::new(),
                file: AI_OVERVIEW.into(),
                title: "概览".into(),
                content: format!("示例项目。依据 `docs/guide.md:L1-L3`。\n## 学习顺序\n1. [仓库导览](TOPIC-tour.md) —— 熟悉项目文件结构。\n"),
                inputs: vec!["docs/guide.md".into()],
                ranges: Vec::new(),
            },
            wb::StagedArticle {
                file: "TOPIC-tour.md".into(),
                focus: String::new(),
                title: "仓库导览".into(),
                // The ONLY place that answers either question is buried
                // under this article's own headings, several bullets below
                // them — verbatim the REA TOPIC-2 shape. Neither command
                // shares wording with its question (验证流程 vs 测试命令).
                content: format!(
                    "## 从哪里开始读代码\n进入 `src/composition/` 目录查看组合层，依据 `src/engine.ts:L1-L3`。\n### 补充\n文档分层说明。\n更多说明。\n验证流程：快速检查用 `npm run check:fast`，完整门禁用 `npm run check:pr`，依据 `docs/guide.md:L1-L3`。\n运行测试：`npm test` 跑全量用例，依据 `docs/guide.md:L1-L3`。\n"
                ),
                inputs: vec!["src/engine.ts".into()],
                ranges: Vec::new(),
            },
        ],
    )
    .unwrap();

    let hits = wiki::evidence(&data, "从哪里开始读代码", 8);
    let tour = hits
        .iter()
        .find(|h| h.page.starts_with("TOPIC-tour.md:"))
        .unwrap_or_else(|| panic!("the article that answers the question must be in its own evidence pack: {hits:?}"));
    assert_eq!(tour.kind.as_deref(), Some("wiki"));
    assert!(tour.excerpt.contains("src/composition"), "the section body carries the answer: {tour:?}");
    // The overview keeps its section behaviour unchanged — its own heading
    // still pulls its section (each page's sections rank on their own terms;
    // the pre-existing max-1 threshold decides who shares a pack).
    let ov = wiki::evidence(&data, "学习顺序是什么", 8);
    assert!(
        ov.iter().any(|h| h.page.starts_with("OVERVIEW.md:") && h.excerpt.contains("仓库导览")),
        "overview sections keep ranking: {ov:?}"
    );

    // Real regression, same shape as REA Q5 (如何测试): the answering bullet
    // sits mid-section and shares no wording with ANY section heading, so
    // only a body-content window can surface it — and the window must reach
    // BACK to the context above the match (the check commands live one
    // bullet above 运行测试, exactly where the real TOPIC-2 puts them).
    let t = wiki::evidence(&data, "如何为这个项目运行测试", 8);
    let win = t
        .iter()
        .find(|h| h.page.starts_with("TOPIC-tour.md:") && h.excerpt.contains("npm test"))
        .unwrap_or_else(|| panic!("a mid-section answer must reach QA through the body window: {t:?}"));
    assert!(
        win.excerpt.contains("check:fast"),
        "the window carries the matching line AND the context above it: {win:?}"
    );
}

/// Real regression (REA acceptance): with both sides hitting, gather used to
/// reserve ONE wiki slot, and a wall of weak raw near-matches (near-duplicate
/// multilingual READMEs all scoring one) ate the rest — the one published
/// article holding the answer never reached the pack. From three slots up,
/// the last two belong to Wiki prose; raw still ranks first and owns the
/// majority; a single slot still goes to raw.
#[test]
fn gather_reserves_wiki_slots_when_a_wall_of_weak_raw_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, "package.json", "{\"name\":\"app\"}");
    for f in ["src/a.ts", "src/b.ts", "docs/c.md"] {
        touch(&ws, f, "export const THETA_MARKER_PHOENIX = 1;\n");
    }
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx,
        &[
            wb::StagedArticle {
                file: "TOPIC-theta.md".into(),
                title: "甲".into(),
                focus: String::new(),
                content: "`THETA_MARKER_PHOENIX` 的定义与用途说明，依据 `src/a.ts:L1-L1`。\n".into(),
                inputs: vec!["src/a.ts".into()],
                ranges: Vec::new(),
            },
            wb::StagedArticle {
                file: "TOPIC-theta-2.md".into(),
                title: "乙".into(),
                content: "`THETA_MARKER_PHOENIX` 的调用方梳理，依据 `src/b.ts:L1-L1`。\n".into(),
                focus: String::new(),
                inputs: vec!["src/b.ts".into()],
                ranges: Vec::new(),
            },
        ],
    )
    .unwrap();

    let four = wiki::gather(&data, &ws, "THETA_MARKER_PHOENIX", 4, 100, 65_536);
    assert_eq!(four.len(), 4, "full pack: {four:?}");
    assert_eq!(four[0].kind.as_deref(), Some("raw"), "raw still ranks first");
    assert_eq!(four[1].kind.as_deref(), Some("raw"), "raw keeps the majority");
    assert_eq!(
        four[2..].iter().filter(|h| h.kind.as_deref() == Some("wiki")).count(),
        2,
        "the last two slots belong to Wiki prose even against three raw hits: {four:?}"
    );

    let two = wiki::gather(&data, &ws, "THETA_MARKER_PHOENIX", 2, 100, 65_536);
    assert_eq!(
        (two.first().and_then(|h| h.kind.as_deref()), two.get(1).and_then(|h| h.kind.as_deref())),
        (Some("raw"), Some("wiki")),
        "two slots split one-and-one: {two:?}"
    );

    let one = wiki::gather(&data, &ws, "THETA_MARKER_PHOENIX", 1, 100, 65_536);
    assert_eq!(one.first().and_then(|h| h.kind.as_deref()), Some("raw"), "the preferred source wins a lone slot");
}

/// Spec 四: 陈旧文章不作为当前事实. When an article's own evidence moved
/// after publication, a hit on that article must announce it on every
/// reader's shared provenance line — the QA prompt, the bridge reply and
/// the UI all render `hit_label`, so the warning travels with the hit.
/// Articles whose evidence still stands must stay unmarked: flagging fresh
/// articles would train readers to ignore the flag.
#[test]
fn stale_article_hits_announce_themselves_fresh_ones_stay_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let (data, ws) = (tmp.path().join("data"), tmp.path().join("ws"));
    touch(&ws, "package.json", "{\"name\":\"app\"}");
    touch(&ws, "src/moved.ts", "export const GAMMA = 1;\n");
    touch(&ws, "docs/stable.md", "GAMMA 的文档梳理\n");
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx,
        &[
            wb::StagedArticle {
                file: "TOPIC-moved.md".into(),
                title: "动了的".into(),
                // Real template articles carry `##` sections (heading/window
                // scoring), unlike a bare line.
                focus: String::new(),
                content: "## GAMMA 机制\n进入实现看导出，依据 `src/moved.ts:L1-L1`。\n".into(),
                inputs: vec!["src/moved.ts".into()],
                ranges: Vec::new(),
            },
            wb::StagedArticle {
                file: "TOPIC-stable.md".into(),
                title: "稳着的".into(),
                content: "## GAMMA 文档\n视角整理自文档，依据 `docs/stable.md:L1-L1`。\n".into(),
                inputs: vec!["docs/stable.md".into()],
                ranges: Vec::new(),
                focus: String::new(),
            },
        ],
    )
    .unwrap();

    // The moved article's own input changes: it now describes a source it
    // never saw. Re-compile so the PERSISTED index — the one every reader
    // judges staleness against — is current.
    touch(&ws, "src/moved.ts", "export const GAMMA = 1;\nexport const DELTA = 2;\n");
    wiki::compile(&data, &ws, 100, 65_536).unwrap();

    let hits = wiki::evidence(&data, "GAMMA", 8);
    let moved = hits
        .iter()
        .find(|h| h.page.starts_with("TOPIC-moved.md:"))
        .expect("the moved article still matches by text — that is exactly why it must be flagged");
    assert!(moved.stale, "moved evidence must surface on the hit: {moved:?}");
    let label = wiki::hit_label(moved);
    assert!(label.contains("可能过期") && label.contains("不作当前事实"), "{label}");

    let stable = hits
        .iter()
        .find(|h| h.page.starts_with("TOPIC-stable.md:"))
        .expect("the untouched article answers too");
    assert!(!stable.stale, "a hit standing on unmoved evidence is not flagged: {stable:?}");
    let label = wiki::hit_label(stable);
    assert!(label.contains("Wiki 文章") && !label.contains("过期"), "{label}");
}
