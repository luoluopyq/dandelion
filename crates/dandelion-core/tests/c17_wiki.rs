//! C17: wiki compile under data_dir.

use dandelion_core::host::CliHost;
use dandelion_core::{Runtime, ScriptedProvider, Settings};
use dandelion_core::provider::ScriptedTurn;
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn runtime(dir: &Path) -> Runtime {
    let host = Arc::new(CliHost::with_data_dir(dir.join("data")));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(dir.join("ws"));
    settings.wiki_compile_on_idle = false;
    settings.cron_tick_sec = 3_600;
    Runtime::new_with_provider(host, settings, Arc::new(ScriptedProvider::new(vec![])))
        .expect("runtime")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compile_show_search_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    fs::create_dir_all(ws.join("src")).unwrap();
    fs::write(ws.join("README.md"), "# Demo\nfeature alpha\n").unwrap();
    fs::write(ws.join("src/lib.rs"), "pub fn alpha(x: i32) -> i32 { x }\n").unwrap();
    fs::write(ws.join("src/orders.ts"), "export function createOrder() { return 1 }\n").unwrap();

    let rt = runtime(dir.path());
    let meta = rt.wiki_compile(Some(&ws)).expect("compile");
    assert!(meta.version >= 1);
    assert_eq!(meta.pages.len(), 4);

    let status = rt.wiki_status();
    assert!(status.has_compiled);
    assert_eq!(status.version, meta.version);

    let pages = rt.wiki_show(Some("ARCHITECTURE.md")).expect("show");
    assert_eq!(pages.len(), 1);
    assert!(!pages[0].body.is_empty());
    let modules = rt.wiki_show(Some("MODULES.md")).unwrap();
    assert!(modules[0].body.contains("createOrder"));

    let hits = rt.wiki_search("alpha");
    assert!(!hits.is_empty(), "expected search hit for alpha");

    let again = rt.wiki_compile(Some(&ws)).expect("recompile");
    assert_eq!(again.version, meta.version + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_overview_and_grounded_answer_are_project_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let alpha = dir.path().join("alpha");
    let beta = dir.path().join("beta");
    fs::create_dir_all(&alpha).unwrap();
    fs::create_dir_all(&beta).unwrap();
    fs::write(alpha.join("README.md"), "# Alpha\n## Alpha模块\nAlpha模块负责检索。\n").unwrap();
    fs::write(beta.join("README.md"), "# Beta\n## Beta模块\nBeta模块负责日历。\n").unwrap();
    let host = Arc::new(CliHost::with_data_dir(dir.path().join("data")));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(alpha.clone());
    let provider = ScriptedProvider::new(vec![
        ScriptedTurn::text("# 项目概览\n## 模块\nAlpha模块负责检索。来源：ARCHITECTURE.md"),
        ScriptedTurn::text("Alpha模块负责检索。[1]"),
    ]);
    let rt = Runtime::new_with_provider(host, settings, Arc::new(provider)).unwrap();
    let meta = rt.wiki_compile_llm(Some(&alpha)).await.unwrap();
    assert!(meta.pages.contains(&"OVERVIEW.md".to_string()));
    let overview = rt.wiki_show(Some("OVERVIEW.md")).unwrap();
    assert!(overview[0].body.contains("Alpha模块"));
    let answer = rt.wiki_ask("Alpha模块负责什么").await.unwrap();
    assert!(answer.answer.contains("检索"));
    assert!(!answer.sources.is_empty());
    assert!(answer.sources.iter().any(|s| s.excerpt.contains("Alpha")));
    assert!(rt.sessions_list(Some(20), 0).unwrap().is_empty(), "internal Wiki runs must not appear as conversations");
    let db = rusqlite::Connection::open(dir.path().join("data/sessions.sqlite")).unwrap();
    let leaked: i64 = db.query_row("SELECT COUNT(*) FROM sessions WHERE kind = 'wiki'", [], |row| row.get(0)).unwrap();
    assert_eq!(leaked, 0, "finished Wiki runs must clean up their temporary session");
    let broad = dandelion_core::wiki::evidence(
        &rt.wiki_dir_for(Some(&alpha)),
        "项目有哪些主要模块", 8,
    );
    assert!(broad.iter().any(|s| s.page.starts_with("OVERVIEW.md:") && s.excerpt.contains("Alpha模块")));

    rt.wiki_compile(Some(&beta)).unwrap();
    assert!(rt.wiki_status_for(Some(&beta)).has_compiled);
    assert!(!rt.wiki_search_for(Some(&beta), "Alpha").iter().any(|h| h.excerpt.contains("Alpha")));
    let missing = rt.wiki_ask_for(Some(&beta), "Alpha订单在哪里").await.unwrap();
    assert!(missing.sources.is_empty());
    assert!(missing.answer.contains("没有找到"));
    assert!(rt.wiki_show(Some("OVERVIEW.md")).unwrap()[0].body.contains("Alpha模块"));
}
