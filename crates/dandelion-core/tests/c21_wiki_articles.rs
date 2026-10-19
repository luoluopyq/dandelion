//! C21: deep-build article lifecycle (Wiki phase S2), end to end through the
//! public Wiki surface. Covers the spec items that cross module boundaries:
//! a failed build keeps the previous Wiki readable and the failure *visible*
//! across quick compiles; editing a source file makes exactly the articles
//! that stood on it stale and an incremental rebuild clears them; a quick
//! scan never deletes published articles; human edits are never overwritten.

use dandelion_core::{wiki, wiki_build as wb, wiki_index as wi};
use std::fs;

fn touch(ws: &std::path::Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{\"name\":\"app\",\"main\":\"src/run.ts\"}");
    touch(&ws, "src/run.ts", "export function launch() {\n  boot();\n}\n");
    touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
    (tmp, data, ws)
}

fn staged(file: &str, title: &str, body: &str, inputs: &[&str]) -> wb::StagedArticle {
    wb::StagedArticle {
        file: file.into(),
        title: title.into(),
        focus: String::new(),
        content: body.into(),
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        ranges: Vec::new(),
    }
}

#[test]
fn failed_build_keeps_previous_wiki_and_the_failure_visible() {
    let (_t, data, ws) = setup();
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();

    // First build succeeds.
    wb::publish_build(
        &data,
        &idx,
        &[staged(
            wiki::AI_OVERVIEW,
            "项目概览",
            "# 概览\n\n入口是 `src/run.ts:L1-L3`。\n",
            &["src/run.ts"],
        )],
    )
    .unwrap();
    assert!(wiki::status(&data).last_build_error.is_none());

    // A failed attempt records stage+reason…
    wb::verify_build(&idx, &[staged("TOPIC-x.md", "T", "编造 `src/none.ts:L9`", &[])])
        .unwrap_err();
    wiki::record_build_error(&data, "verify", "《T》: 引用了证据索引之外的文件：src/none.ts");
    let st = wiki::status(&data);
    let err = st.last_build_error.expect("failure visible in status");
    assert_eq!(err.stage, "verify");

    // …and survives a quick compile (spec D: keep showing why).
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let st = wiki::status(&data);
    assert_eq!(st.last_build_error.as_ref().unwrap().stage, "verify");
    // previous published article still readable
    let overview = wiki::show(&data, Some(wiki::AI_OVERVIEW)).unwrap();
    assert!(overview[0].body.contains("src/run.ts"));

    // A successful later build clears it.
    wb::publish_build(
        &data,
        &idx,
        &[staged(
            "TOPIC-run.md",
            "启动流程",
            "`launch` 在 `src/run.ts:L1-L3`。\n",
            &["src/run.ts"],
        )],
    )
    .unwrap();
    assert!(wiki::status(&data).last_build_error.is_none());
}

#[test]
fn stale_detection_and_incremental_rebuild_over_the_real_flow() {
    let (_t, data, ws) = setup();
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx0 = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx0,
        &[
            staged(wiki::AI_OVERVIEW, "项目概览", "# 概览\n\n`src/run.ts:L1-L3`\n", &["src/run.ts"]),
            staged("TOPIC-docs.md", "文档", "# 文档\n\n运行方式 `docs/guide.md:L3-L5`\n", &["docs/guide.md"]),
        ],
    )
    .unwrap();
    assert!(wb::stale_article_files(&data, &idx0).is_empty());

    // Modify a key source file → only articles standing on it are stale.
    touch(&ws, "src/run.ts", "export function launch() {\n  bootV2(); // reworked\n}\n");
    let idx1 = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    let stale = wb::stale_article_files(&data, &idx1);
    assert_eq!(stale, vec![wiki::AI_OVERVIEW.to_string()], "only the overview depends on src/run.ts");

    // Incremental rebuild of just the stale set clears staleness.
    wb::publish_build(
        &data,
        &idx1,
        &[staged(wiki::AI_OVERVIEW, "项目概览", "# 概览\n\n现在入口 `src/run.ts:L1-L3`\n", &["src/run.ts"])],
    )
    .unwrap();
    assert!(wb::stale_article_files(&data, &idx1).is_empty());
    // untouched article keeps its original build number
    let reg = wb::load_articles(&data);
    assert_eq!(reg.articles["TOPIC-docs.md"].build, 1);
    assert_eq!(reg.articles[wiki::AI_OVERVIEW].build, 2);
}

#[test]
fn scan_never_deletes_articles_and_never_overwrites_user_edits() {
    let (_t, data, ws) = setup();
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let idx = wi::build_index(&data, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &data,
        &idx,
        &[
            staged(wiki::AI_OVERVIEW, "项目概览", "机读 `src/run.ts:L1-L3`\n", &["src/run.ts"]),
            staged("TOPIC-run.md", "启动流程", "机器 `src/run.ts:L1-L3`\n", &["src/run.ts"]),
        ],
    )
    .unwrap();
    // Human corrects one article through the article-level API.
    fs::write(wiki::compiled_dir(&data).join("TOPIC-run.md"), "人工修订的结论 `src/run.ts:L1`\n").unwrap();
    assert!(wb::mark_user_edited(&data, "TOPIC-run.md"));

    // Quick scan + a full rebuild attempt: nothing human is lost, nothing
    // published is deleted.
    wiki::compile(&data, &ws, 100, 65_536).unwrap();
    let out = wb::publish_build(
        &data,
        &idx,
        &[
            staged(wiki::AI_OVERVIEW, "项目概览", "机读V2 `src/run.ts:L1-L3`\n", &["src/run.ts"]),
            staged("TOPIC-run.md", "启动流程", "机器V2 `src/run.ts:L1-L3`\n", &["src/run.ts"]),
        ],
    )
    .unwrap();
    assert_eq!(out.skipped_user_edited, vec!["TOPIC-run.md".to_string()]);
    let body = fs::read_to_string(wiki::compiled_dir(&data).join("TOPIC-run.md")).unwrap();
    assert!(body.contains("人工修订"), "user knowledge must not be overwritten: {body}");
    assert!(wiki::compiled_dir(&data).join("TOPIC-run.md").is_file());

    // The reading surface shows every article with the right identity.
    let pages = wiki::effective_pages(&data);
    assert!(pages.iter().any(|p| p.file == "TOPIC-run.md" && p.role.as_deref() == Some("model")));
    assert!(pages.iter().any(|p| p.file == wiki::AI_OVERVIEW && p.role.as_deref() == Some("overview")));
}
