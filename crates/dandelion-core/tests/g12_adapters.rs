//! G12: source adapters (unification phase 3).
//!
//! The honest-extraction contract, pinned: line-chunked text with clickable
//! `#Lx-Ly` locators, a byte budget that truncates visibly instead of
//! silently, unreadable input reported (never fabricated, never a crash
//! that takes the whole build down), a directory source as its tree
//! listing, a web page as its body-text snapshot with line-range locators,
//! a capability list that names its own gaps. These are unit-level
//! adapter gates; real-PDF and real-model behaviour belongs to the real
//! acceptance round and is not claimed here.

use dandelion_core::wiki_adapters as wa;
use std::fs;

fn touch(dir: &std::path::Path, rel: &str, body: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn text_adapter_chunks_with_line_locators() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lines = String::new();
    for i in 1..=400 {
        lines.push_str(&format!("line {i}\n"));
    }
    touch(tmp.path(), "guide.md", lines.as_bytes());

    let out = wa::extract_source(&tmp.path().join("guide.md"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "text");
    assert_eq!(out.chunks.len(), 3, "160/160/80: {:?}", out.chunks.len());
    assert_eq!(out.chunks[0].locator, "guide.md#L1-L160");
    assert_eq!(out.chunks[1].locator, "guide.md#L161-L320");
    assert_eq!(out.chunks[2].locator, "guide.md#L321-L400");
    assert!(out.chunks[2].text.starts_with("line 321\n"));
    // ok_bytes counts exactly what the chunks carry (chunk boundaries
    // between them are not part of any chunk).
    let carried: u64 = out.chunks.iter().map(|c| c.text.len() as u64).sum();
    assert_eq!(out.report.ok_bytes, carried);
    assert!(carried >= lines.len() as u64 - 3, "{carried} vs {}", lines.len());
    assert!(!out.report.truncated);
    assert!(out.report.skipped.is_empty(), "{:?}", out.report.skipped);
}

#[test]
fn text_adapter_truncates_visibly_at_char_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    // Chinese body: a byte cap will land mid-character unless the adapter
    // floors to a boundary — it must still succeed, visibly truncated.
    let body = "知识空间统一模块。".repeat(200);
    touch(tmp.path(), "notes.txt", body.as_bytes());

    let out = wa::extract_source(&tmp.path().join("notes.txt"), 100).unwrap();
    assert_eq!(out.source_kind, "text");
    assert!(out.report.truncated, "budget hit must be reported");
    assert!(!out.report.skipped.is_empty(), "truncation names itself: {:?}", out.report.skipped);
    assert!(!out.chunks[0].text.is_empty(), "a truncated read still yields head content");
    assert!(out.chunks[0].text.len() <= 100);
}

#[test]
fn unreadable_text_is_reported_not_fabricated() {
    let tmp = tempfile::tempdir().unwrap();
    // 0xFF/0xFE start no valid UTF-8 sequence at all — genuinely broken.
    touch(tmp.path(), "broken.md", &[0xff, 0xfe, 0xff, 0xfe, 0x00, 0x01]);
    let out = wa::extract_source(&tmp.path().join("broken.md"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "text");
    assert!(out.chunks.is_empty(), "no text invented for broken input");
    assert!(
        out.report.skipped.iter().any(|s| s.contains("UTF-8")),
        "{:?}",
        out.report.skipped
    );

    touch(tmp.path(), "empty.md", b"");
    let out = wa::extract_source(&tmp.path().join("empty.md"), 1_000_000).unwrap();
    assert!(out.chunks.is_empty());
    assert!(!out.report.skipped.is_empty(), "empty is also accounted for");
}

#[test]
fn directory_source_is_a_tree_listing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("资料");
    touch(&root, "b.md", b"# b\n");
    touch(&root, "a.txt", b"a\n");
    touch(&root, "sub/deep.md", b"# deep\n");

    let out = wa::extract_source(&root, 1_000_000).unwrap();
    assert_eq!(out.source_kind, "dir_tree");
    assert_eq!(out.chunks.len(), 1);
    assert_eq!(out.chunks[0].locator, "资料#tree");
    let listing = out.chunks[0].text.lines().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(listing, vec!["a.txt", "b.md", "sub/", "sub/deep.md"], "{listing:?}");
    assert!(out.report.skipped.is_empty());
}

#[test]
fn broken_pdf_reports_failure_and_never_crashes() {
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "fake.pdf", b"this is not a pdf, not even close");
    let out = wa::extract_source(&tmp.path().join("fake.pdf"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "pdf");
    assert!(out.chunks.is_empty(), "a broken PDF contributes nothing — and says so");
    // Honest either way it fails: a parse error or an empty extraction is
    // reported with the reason — what must never happen is silence.
    assert!(
        out.report.skipped.iter().any(|s| s.contains("PDF")),
        "{:?}",
        out.report.skipped
    );
}

#[test]
fn html_adapter_snapshots_the_body_with_line_locators() {
    let tmp = tempfile::tempdir().unwrap();
    let page = r#"<!DOCTYPE html>
<html><head>
<meta charset="utf-8"><title>注塑工艺</title>
<style>body { margin: 0 }</style>
</head>
<body>
<h1>注塑工艺</h1>
<script>var x = "</div>"; if (1<2) { log(x); }</script>
<p>保压时间&nbsp;不足会出现缩痕 &amp; 飞边。</p>
<div>
<section><h2>缺陷</h2><ul><li>缩痕</li><li>飞边</li></ul></section>
<p title="a>b">属性里的尖括号不算正文。</p>
</div>
<p>&#38450;水&#x8272;</p>
</body></html>"#;
    touch(tmp.path(), "page.html", page.as_bytes());

    let out = wa::extract_source(&tmp.path().join("page.html"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "html");
    assert!(out.report.skipped.is_empty(), "{:?}", out.report.skipped);
    // One snapshot line per visible block; scripts and styles are never
    // page text; entities decode; a quoted `>` inside an attribute stays
    // inside the attribute.
    let got = out.chunks[0].text.lines().collect::<Vec<_>>();
    assert_eq!(
        got,
        vec![
            "注塑工艺", // head is NOT blind-skipped: `</head>` is optional in real HTML
            "注塑工艺",
            "保压时间 不足会出现缩痕 & 飞边。",
            "缺陷",
            "缩痕",
            "飞边",
            "属性里的尖括号不算正文。",
            "防水色",
        ],
        "{got:?}"
    );
    // Snapshot line ranges — the same clickable locator shape text sources
    // get, which is what makes 网页快照及章节 citable by the existing layer.
    assert_eq!(out.chunks[0].locator, "page.html#L1-L8");
}

#[test]
fn html_adapter_reports_what_it_cannot_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    // A page that is nothing but scripts and styles has no body text —
    // reported, not fabricated.
    touch(
        tmp.path(),
        "shell.html",
        b"<!doctype html><html><head><script>var a=1;</script></head><body></body></html>",
    );
    let out = wa::extract_source(&tmp.path().join("shell.html"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "html");
    assert!(out.chunks.is_empty(), "a script shell contributes nothing — and says so");
    assert!(
        out.report.skipped.iter().any(|s| s.contains("正文")),
        "{:?}",
        out.report.skipped
    );

    // An unclosed <script> swallows only to end of input; what came before
    // it is still snapshotted.
    touch(
        tmp.path(),
        "cut.html",
        "<p>先有正文</p><script>while(1){".as_bytes(),
    );
    let out = wa::extract_source(&tmp.path().join("cut.html"), 1_000_000).unwrap();
    assert_eq!(out.chunks[0].text, "先有正文");

    // The byte budget truncates visibly, exactly like the text adapter.
    touch(
        tmp.path(),
        "big.html",
        format!("<p>{}</p>", "页面".repeat(500)).as_bytes(),
    );
    let out = wa::extract_source(&tmp.path().join("big.html"), 40).unwrap();
    assert!(out.report.truncated, "budget hit must be reported");
    assert!(
        out.report.skipped.iter().any(|s| s.contains("字节预算")),
        "{:?}",
        out.report.skipped
    );
    assert!(!out.chunks.is_empty(), "head content survives the truncation");
}

#[test]
fn html_pages_count_as_a_capability() {
    let kinds = wa::capability_kinds();
    assert!(kinds.contains(&"html"), "网页正文快照 is a named capability: {kinds:?}");
    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "x.htm", b"<p>hi</p>");
    assert!(wa::probe(&tmp.path().join("x.htm")).is_some());
    // Probe is by extension only, case-insensitively.
    assert!(wa::probe(&tmp.path().join("Y.HTML")).is_some());
    assert!(wa::probe(&tmp.path().join("y.zip")).is_none());
}

#[test]
fn capability_list_names_its_gaps() {
    let kinds = wa::capability_kinds();
    assert!(kinds.contains(&"text") && kinds.contains(&"pdf") && kinds.contains(&"dir_tree"), "{kinds:?}");

    let tmp = tempfile::tempdir().unwrap();
    touch(tmp.path(), "archive.zip", b"PK\x03\x04junk");
    let out = wa::extract_source(&tmp.path().join("archive.zip"), 1_000_000).unwrap();
    assert_eq!(out.source_kind, "unsupported");
    assert!(out.chunks.is_empty());
    assert!(
        out.report.skipped.iter().any(|s| s.contains("zip") && s.contains("暂不支持")),
        "the miss is a named gap: {:?}",
        out.report.skipped
    );
    // probe agrees with extract_source
    assert!(wa::probe(&tmp.path().join("x.md")).is_some());
    assert!(wa::probe(&tmp.path().join("x.zip")).is_none());
}
