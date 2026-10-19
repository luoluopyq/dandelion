//! C19: reading-first Wiki contract — the page registry is the UI's only
//! source for page identity, order, titles, TOC groups and page roles.
//! Covers: unbuilt status, honest scan-page titles, overview role, legacy
//! registries without metadata, review/undo page churn surfacing correctly.

use dandelion_core::wiki::{
    self, AI_OVERVIEW, COMPILED_APIS, COMPILED_ARCH, COMPILED_FEATURES, COMPILED_MODULES,
    ENTRY_STATUS_ACCEPTED, ENTRY_STATUS_PENDING, GROUP_DISTILLED, GROUP_SCAN, OUTLINE_PAGE,
    PAGE_DECISIONS, ProposedEntry, ROLE_DISTILLED, ROLE_OVERVIEW, ROLE_SCAN,
};
use std::fs;

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn code_ws(ws: &std::path::Path) {
    fs::create_dir_all(ws.join("src")).unwrap();
    fs::write(ws.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(ws.join("README.md"), "# Demo\n\nA demo crate.\n").unwrap();
    fs::write(ws.join("src/lib.rs"), "pub fn hello() {}\n").unwrap();
}

#[test]
fn unbuilt_project_reports_no_pages_and_no_compile() {
    let tmp = scratch();
    let data = tmp.path().join("data");
    let status = wiki::status(&data);
    assert!(!status.has_compiled);
    assert!(status.pages.is_empty(), "{:?}", status.pages);
}

#[test]
fn scan_pages_have_honest_titles_roles_and_groups() {
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    code_ws(&ws);
    wiki::compile(&data, &ws, 50, 65_536).unwrap();

    let pages = wiki::effective_pages(&data);
    let titles: Vec<(&str, &str)> = pages
        .iter()
        .map(|p| (p.file.as_str(), p.title.as_str()))
        .collect();
    // Titles describe what the deterministic scan actually produces.
    assert_eq!(
        titles,
        vec![
            (COMPILED_ARCH, "仓库构成"),
            (COMPILED_MODULES, "源码结构"),
            (COMPILED_FEATURES, "文档要点"),
            (COMPILED_APIS, "公开函数"),
        ]
    );
    for page in &pages {
        assert_eq!(page.role.as_deref(), Some(ROLE_SCAN), "{page:?}");
        assert_eq!(page.group.as_deref(), Some(GROUP_SCAN), "{page:?}");
    }
    // The overview page is registered but absent until a model build writes
    // its body — the UI must not show a default page with no content.
    let reg = wiki::load_registry(&data);
    assert!(reg.pages.iter().any(|p| p.file == AI_OVERVIEW));
    assert!(!pages.iter().any(|p| p.file == AI_OVERVIEW));

    // Status (what the panel renders) carries the same metadata.
    let status = wiki::status(&data);
    assert!(status.has_compiled);
    assert_eq!(status.pages, pages);
}

#[test]
fn model_overview_becomes_the_default_reading_page_by_role() {
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    code_ws(&ws);
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    // Simulate the model-build step writing the overview body.
    fs::write(wiki::compiled_dir(&data).join(AI_OVERVIEW), "# 项目概览\n\n说明。\n").unwrap();

    let pages = wiki::effective_pages(&data);
    let overview = pages
        .iter()
        .find(|p| p.role.as_deref() == Some(ROLE_OVERVIEW))
        .expect("overview page carries its role from the registry");
    assert_eq!(overview.file, AI_OVERVIEW);
    assert_eq!(overview.title, "项目概览");
}

#[test]
fn legacy_registry_without_metadata_still_reads_and_backfills_roles() {
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    code_ws(&ws);
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    // Rewrite pages.json exactly as an older build would have: file+title only,
    // with a user-visible custom title and the old (over-promising) title text.
    fs::write(
        wiki::registry_path(&data),
        r#"{"pages":[{"file":"ARCHITECTURE.md","title":"架构"},{"file":"MODULES.md","title":"我的模块"}]}"#,
    )
    .unwrap();
    let pages = wiki::effective_pages(&data);
    let arch = pages.iter().find(|p| p.file == COMPILED_ARCH).unwrap();
    // Registry title survives (it is the user-visible truth until recompile),
    // but role/group are backfilled from known-page defaults, so the reading
    // UI can still group and default-select on old projects.
    assert_eq!(arch.title, "架构");
    assert_eq!(arch.role.as_deref(), Some(ROLE_SCAN));
    assert_eq!(arch.group.as_deref(), Some(GROUP_SCAN));
    assert_eq!(
        pages.iter().find(|p| p.file == COMPILED_MODULES).unwrap().title,
        "我的模块"
    );

    // The next compile re-titles profile pages (force_meta) with honest names.
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    let pages = wiki::effective_pages(&data);
    assert_eq!(
        pages.iter().find(|p| p.file == COMPILED_ARCH).unwrap().title,
        "仓库构成"
    );
    // A non-profile page keeps its custom title across compiles.
    fs::write(wiki::compiled_dir(&data).join("EXTRA.md"), "x\n").unwrap();
    let mut reg = wiki::load_registry(&data);
    reg.pages.push(wiki::WikiPageEntry {
        file: "EXTRA.md".into(),
        title: "我的补充页".into(),
        group: None,
        role: None,
        ..Default::default()
    });
    wiki::save_registry(&data, &reg).unwrap();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    assert!(wiki::effective_pages(&data)
        .iter()
        .any(|p| p.file == "EXTRA.md" && p.title == "我的补充页"));
}

#[test]
fn review_and_undo_move_entries_onto_and_off_pages() {
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    code_ws(&ws);
    wiki::compile(&data, &ws, 50, 65_536).unwrap();

    let proposal = ProposedEntry {
        kind: "decision".into(),
        content: "先出样品再报价".into(),
        session_id: "sess-reading-1".into(),
        turn: Some(3),
        revises: None,
        idempotency_key: None,
    };
    // A pending proposal never reaches a page, no matter how many compiles run.
    let report = wiki::ingest_entries(&data, &[proposal], ENTRY_STATUS_PENDING, 4_000).unwrap();
    let id = report.added[0].id.clone();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    assert!(!wiki::effective_pages(&data).iter().any(|p| p.file == PAGE_DECISIONS));

    // Approve → compile puts it on the page, tagged distilled.
    wiki::set_entry_status(&data, &id, ENTRY_STATUS_ACCEPTED).unwrap();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    let page = wiki::show(&data, Some("DECISIONS")).unwrap();
    assert!(page[0].body.contains("先出样品再报价"), "{}", page[0].body);
    let distilled = wiki::effective_pages(&data)
        .into_iter()
        .find(|p| p.file == PAGE_DECISIONS)
        .expect("decisions page surfaces after approval");
    assert_eq!(distilled.role.as_deref(), Some(ROLE_DISTILLED));
    assert_eq!(distilled.group.as_deref(), Some(GROUP_DISTILLED));

    // Undo → compile takes the page down with it (no stale page left behind).
    wiki::remove_distilled(&data, &id).unwrap();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    assert!(!wiki::effective_pages(&data).iter().any(|p| p.file == PAGE_DECISIONS));
    assert!(!wiki::compiled_dir(&data).join(PAGE_DECISIONS).is_file());
}

#[test]
fn generic_profile_replaces_code_pages_and_retitles_features() {
    let tmp = scratch();
    let ws = tmp.path().join("ws");
    let data = tmp.path().join("data");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("plan.md"), "# 计划\n## 渠道\n线上\n").unwrap();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    assert!(wiki::compiled_dir(&data).join(OUTLINE_PAGE).is_file());
    assert!(!wiki::compiled_dir(&data).join(COMPILED_ARCH).is_file());
    let pages = wiki::effective_pages(&data);
    assert!(pages
        .iter()
        .any(|p| p.file == COMPILED_FEATURES && p.title == "要点"));

    // Switch to code: old code-free pages are replaced and titles refreshed.
    wiki::save_profile(&data, &wiki::WikiProfile::code()).unwrap();
    wiki::compile(&data, &ws, 50, 65_536).unwrap();
    let pages = wiki::effective_pages(&data);
    assert!(!pages.iter().any(|p| p.file == OUTLINE_PAGE));
    assert!(pages
        .iter()
        .any(|p| p.file == COMPILED_FEATURES && p.title == "文档要点"));
}
