//! C24: the corpus compatibility surface stays frozen across the phase-5
//! storage merge (work order: 杜绝双写但不破坏现有调用方).
//!
//! Three layers of the same call path must not drift:
//! `Runtime::corpus_{list,import,delete}` (compile-time signatures),
//! the Tauri commands (`corpus_list` / `corpus_import_pick` /
//! `corpus_delete` calling straight through), and the ipc.ts wrappers the
//! Vue store imports. The retrieval segment marker is the user-segment
//! discipline — corpus text enters the model as a user turn, never system.

use dandelion_core::{CorpusDoc, Result, Runtime};
use std::path::Path;

/// Compile-time freeze: these are exactly the signatures the desktop calls.
/// Any change (arguments, return type) breaks this assignment, not the user.
#[test]
fn runtime_corpus_signatures_are_frozen() {
    let list: fn(&Runtime) -> Result<Vec<CorpusDoc>> = Runtime::corpus_list;
    let import: fn(&Runtime, &Path) -> Result<CorpusDoc> = Runtime::corpus_import;
    let del: fn(&Runtime, &str) -> Result<()> = Runtime::corpus_delete;
    let _ = (list, import, del);
}

fn repo_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn tauri_commands_route_straight_to_runtime() {
    let rust = repo_file("../../apps/desktop/src-tauri/src/lib.rs");
    for cmd in ["corpus_list", "corpus_import_pick", "corpus_delete"] {
        assert!(
            rust.contains(&format!("async fn {cmd}(")),
            "Tauri command {cmd} vanished"
        );
        // Registered with the invoke handler exactly once by name.
        assert!(
            rust.contains(&format!("{cmd},\n")) || rust.contains(&format!("\"{cmd}\",")),
            "Tauri registration for {cmd} vanished"
        );
    }
    assert!(rust.contains("rt.corpus_list()"), "corpus_list no longer delegates to Runtime");
    assert!(rust.contains("rt.corpus_import(&path)"), "corpus_import no longer delegates to Runtime");
    assert!(rust.contains("rt.corpus_delete(&id)"), "corpus_delete no longer delegates to Runtime");
}

#[test]
fn ipc_ts_wrappers_call_the_same_three_commands() {
    let ts = repo_file("../../apps/desktop/src/lib/ipc.ts");
    assert!(ts.contains("export function corpusList"), "corpusList wrapper vanished");
    assert!(ts.contains("invoke(\"corpus_list\")"));
    assert!(ts.contains("export function corpusImportPick"), "corpusImportPick wrapper vanished");
    assert!(ts.contains("invoke(\"corpus_import_pick\")"));
    assert!(ts.contains("export function corpusDelete"), "corpusDelete wrapper vanished");
    assert!(ts.contains("invoke(\"corpus_delete\", { id })"));
}

#[test]
fn corpus_text_is_user_segment_content() {
    // The marker string is part of the on-wire prompt shape; renaming it
    // would silently change every prompt built from corpus hits.
    assert_eq!(dandelion_core::corpus::SEGMENT_MARK, "[知识库检索]");
    // And the runtime still builds the segment through the corpus helper,
    // which the loop injects strictly as a ChatMessage::User.
    let rt = repo_file("src/runtime.rs");
    assert!(rt.contains("corpus_segment_for"), "corpus segment builder unhooked from runtime");
    let loop_src = repo_file("src/agent_loop/mod.rs");
    assert!(loop_src.contains("fn with_corpus_user_segment"), "corpus user-segment injection unhooked");
    let fn_body = loop_src
        .split("fn with_corpus_user_segment")
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .unwrap_or_default();
    assert!(
        fn_body.contains("ChatMessage::User"),
        "corpus segment must enter as a user message"
    );
    assert!(
        !fn_body.contains("ChatMessage::System"),
        "corpus segment must never enter as a system message"
    );
}
