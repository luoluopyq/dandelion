//! G13: evidence locators as a first-class enum (unification phase 4).
//!
//! The contract pinned here: the legacy `rel:Lx-Ly` token parses with the
//! exact old rules (legacy-first — no new syntax may steal a legacy token),
//! every display form round-trips through `Display`, unknown shapes degrade
//! to `Unknown` instead of panicking or vanishing, and the line-range read
//! route serves exactly the variants a line fetcher can honestly answer.
//! The c20 suite remains the byte-for-byte regression red line for the
//! citation pipeline itself.

use dandelion_core::wiki_build::parse_locator_tokens;
use dandelion_core::wiki_locator::EvidenceLocator as L;

#[test]
fn legacy_token_parses_verbatim_first() {
    // The old contract: range, single line, and the rejection rules.
    assert_eq!(
        L::from_display("src/a.ts:L10-L20"),
        L::Legacy { rel: "src/a.ts".into(), start: 10, end: 20 }
    );
    assert_eq!(
        L::from_display("src/a.ts:L2"),
        L::Legacy { rel: "src/a.ts".into(), start: 2, end: 2 }
    );
    // Rejections the old parser had — still rejections, degrade honestly.
    assert!(matches!(L::from_display("../secret:L1-L2"), L::Unknown { .. }));
    assert!(matches!(L::from_display("/abs/f.ts:L1-L2"), L::Unknown { .. }));
    assert!(matches!(L::from_display("weird:weird:L1-L2"), L::Unknown { .. }));
    assert!(matches!(L::from_display("f.ts:Lx-L2"), L::Unknown { .. }));
    // Legacy-first: even a shape a new rule could read stays Legacy when
    // the legacy parser claims it.
    assert!(matches!(L::from_display("a.md:L1-L2"), L::Legacy { .. }));
}

#[test]
fn adapter_display_forms_are_first_class() {
    // Phase 3 adapter output is now a parsed value, not just a string.
    assert_eq!(
        L::from_display("guide.md#L1-L160"),
        L::Line { source: "guide.md".into(), start: 1, end: 160 }
    );
    assert_eq!(
        L::from_display("guide.md#L7"),
        L::Line { source: "guide.md".into(), start: 7, end: 7 }
    );
    assert_eq!(L::from_display("spec.pdf#pdf"), L::Anchor { source: "spec.pdf".into(), anchor: "pdf".into() });
    assert_eq!(L::from_display("资料#tree"), L::Anchor { source: "资料".into(), anchor: "tree".into() });
    assert_eq!(L::from_display("spec.pdf#p12"), L::Page { source: "spec.pdf".into(), page: 12 });
    assert_eq!(
        L::from_display("notes.txt#C10-C40"),
        L::Char { source: "notes.txt".into(), start: 10, end: 40 }
    );
}

#[test]
fn web_session_and_anchor_forms() {
    assert_eq!(
        L::from_display("https://example.com/docs#install"),
        L::UrlFragment {
            url: "https://example.com/docs".into(),
            fragment: Some("install".into())
        }
    );
    assert_eq!(
        L::from_display("http://example.com/a"),
        L::UrlFragment { url: "http://example.com/a".into(), fragment: None }
    );
    assert_eq!(L::from_display("session:s123#t42"), L::SessionTurn { session: "s123".into(), turn: 42 });
    assert_eq!(L::from_display("msg:s123#42"), L::SessionTurn { session: "s123".into(), turn: 42 });
    assert_eq!(
        L::from_display("note.md@sec:安装"),
        L::Anchor { source: "note.md".into(), anchor: "sec:安装".into() }
    );
}

#[test]
fn display_round_trips_every_variant() {
    let cases = [
        "src/a.ts:L10-L20",
        "src/a.ts:L2",
        "guide.md#L1-L160",
        "guide.md#L7",
        "notes.txt#C10-C40",
        "spec.pdf#p12",
        "https://example.com/docs#install",
        "http://example.com/a",
        "session:s123#t42",
        "note.md@sec:安装",
        "spec.pdf#pdf",
        "资料#tree",
    ];
    for c in cases {
        let loc = L::from_display(c);
        assert!(loc.is_known(), "{c} must parse, got Unknown");
        assert_eq!(loc.display(), c, "round-trip");
        // And stable: re-parsing the display text is idempotent.
        assert_eq!(L::from_display(&loc.display()), loc);
    }
    // msg form normalizes to the session display, then stays stable.
    let m = L::from_display("msg:s9#3");
    assert_eq!(m.display(), "session:s9#t3");
    assert_eq!(L::from_display(&m.display()), m);
    // Unknown displays itself.
    assert_eq!(L::from_display("你好世界").display(), "你好世界");
}

#[test]
fn unknown_degrades_never_panics() {
    for junk in [
        "", "#", "a#", "#x", "a##b", "@", "a@", "@b", "::::", "session:", "session:s1#notaturn",
        "https://", "msg:#5", "a@b@c", "plain text", "L1-L2", "a.md#Lx", "a.md#pX",
    ] {
        let loc = L::from_display(junk);
        assert!(
            !loc.is_known() || matches!(loc, L::Anchor { .. } | L::UrlFragment { .. }),
            "{junk:?} parsed as {loc:?} — a shape with no source should not be a range variant"
        );
        assert!(!loc.display().is_empty() || junk.is_empty());
    }
}

#[test]
fn line_route_serves_exactly_the_line_variants() {
    assert_eq!(L::from_display("a.ts:L1-L2").as_line_range(), Some(("a.ts", 1, 2)));
    assert_eq!(L::from_display("a.md#L1-L2").as_line_range(), Some(("a.md", 1, 2)));
    // Everything else has no honest line route yet (source table is phase 5).
    for other in ["spec.pdf#p12", "notes.txt#C1-C2", "dir#tree", "session:s1#t1", "https://x.dev/a", "n@sec:1"] {
        assert_eq!(L::from_display(other).as_line_range(), None, "{other}");
    }
}

#[test]
fn citation_text_extracts_locators_with_spans() {
    // Mixed prose: legacy and adapter syntax side by side; spans let a
    // renderer splice clickable runs back into the original text.
    let body = "入口见 `src/a.ts:L1-L3`，结构见 资料#tree，第 12 页 spec.pdf#p12。";
    let found = parse_locator_tokens(body);
    let known: Vec<String> = found
        .iter()
        .filter(|(_, _, l)| l.is_known())
        .map(|(_, _, l)| l.display())
        .collect();
    assert_eq!(known, vec!["src/a.ts:L1-L3", "资料#tree", "spec.pdf#p12"], "{known:?}");
    // Spans point back at the exact substrings.
    for (s, e, l) in &found {
        if l.is_known() {
            assert_eq!(&body[*s..*e], l.display(), "span must contain the token");
        }
    }
    // Legacy citation verification semantics are untouched: parse_citations
    // still only sees the legacy token (c20 red line, spot-checked here).
    let legacy: Vec<_> = dandelion_core::wiki_build::parse_citations(body)
        .into_iter()
        .map(|(r, a, b)| format!("{r}:{a}-{b}"))
        .collect();
    assert_eq!(legacy, vec!["src/a.ts:1-3"]);
}
