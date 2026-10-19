//! G9: immutable version directories + atomic current-pointer (任务四).
//! The Wiki's visible state is `wiki/versions/<n>/` selected by a single
//! pointer file `wiki/current.json`, flipped by ONE rename. This suite proves
//! on THIS OS (Windows) the properties the design leans on:
//! - `std::fs::rename` replaces an existing file in place (the pointer flip
//!   is exactly that — proven empirically, not assumed);
//! - the legacy `wiki/compiled/` layout reads as version 0 and upgrades on
//!   the first commit, its bytes never rewritten;
//! - every page file — user edits and distilled knowledge included — rides
//!   byte-for-byte into each new version via the carry-forward;
//! - GC keeps the newest K versions plus whatever is current, never the
//!   current one;
//! - a REAL child process killed inside each commit window leaves a Wiki
//!   that reads exactly as the old version until [`wb::recover_publish`]
//!   finishes the story — journals handcrafted by a test prove the recovery
//!   code only; these kills prove the WRITE ORDERING.

use dandelion_core::wiki;
use dandelion_core::wiki_build as wb;
use dandelion_core::wiki_index as wi;
use std::fs;
use std::path::{Path, PathBuf};

// ------------------------------------------------------------------ harness

fn touch(ws: &Path, rel: &str, body: &str) {
    let p = ws.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let ws = tmp.path().join("ws");
    touch(&ws, "package.json", "{\"name\":\"app\",\"main\":\"src/run.ts\"}");
    touch(&ws, "src/run.ts", "export function launch() {\n  boot();\n}\n");
    touch(&ws, "docs/guide.md", "# Guide\n\n## Run\n\nnpm start\n");
    fs::create_dir_all(&data).unwrap();
    let pdir = resolved(&data, &ws);
    (tmp, data, ws, pdir)
}

/// The space-resolved Wiki home — the same resolution the runtime uses
/// (post phase-2 migration a registered workspace lives under
/// `wiki/spaces/<id>`, so assertions must read the resolved dir too).
/// Parked children resolve through the same registry, so parent and child
/// always agree on the Wiki home.
fn resolved(data: &std::path::Path, ws: &std::path::Path) -> std::path::PathBuf {
    let store = dandelion_core::store::sqlite::Store::open(data, 5000).unwrap();
    dandelion_core::wiki_space_migrate::ensure_space_dir(&store, data, ws, true).unwrap()
}

fn art(file: &str, title: &str, content: &str) -> wb::StagedArticle {
    wb::StagedArticle {
        file: file.into(),
        title: title.into(),
        focus: String::new(),
        content: content.into(),
        inputs: vec!["docs/guide.md".into()],
        ranges: Vec::new(),
    }
}

fn publish(pdir: &Path, ws: &Path, file: &str, title: &str, content: &str) {
    let index = wi::build_index(pdir, ws, 100, 65_536).unwrap();
    wb::publish_build(pdir, &index, &[art(file, title, content)]).unwrap();
}

/// Sorted `*.md` names inside a directory (never `version.json`).
fn mds(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".md"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Numeric directory names under `wiki/versions/`.
fn version_dirs(pdir: &Path) -> Vec<u64> {
    let mut v: Vec<u64> = fs::read_dir(wiki::versions_root(pdir))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_string_lossy().parse::<u64>().ok())
                .collect()
        })
        .unwrap_or_default();
    v.sort_unstable();
    v
}

// ------------------------------------------------------------------ rename

/// The pointer flip is a plain rename onto an EXISTING file. On POSIX that
/// is atomic replacement by definition; on Windows it needs
/// MoveFileExW(MOVEFILE_REPLACE_EXISTING) semantics — so prove it HERE, on
/// the OS the tests actually run on, twice: raw std, and through two real
/// pointer flips.
#[test]
fn std_rename_replaces_an_existing_file_on_this_os() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("pointer.json");
    let tmpfile = tmp.path().join("pointer.json.tmp");
    fs::write(&target, b"{\"old\":true}").unwrap();
    fs::write(&tmpfile, b"{\"version\":7}").unwrap();
    fs::rename(&tmpfile, &target).expect("rename must replace the existing file on this OS");
    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"version\":7}");
    assert!(!tmpfile.exists(), "the source of the rename is gone");
}

#[test]
fn the_pointer_flips_in_place_with_full_metadata_and_no_tmp_residue() {
    let (t, _data, ws, pdir) = setup();
    publish(&pdir, &ws, "TOPIC-1.md", "甲", "第一版，依据 `docs/guide.md:L1-L3`。\n");
    let p1 = wiki::current_pointer(&pdir).expect("pointer after first publish");
    assert_eq!((p1.version, p1.build, p1.kind.as_str()), (1, 1, "build"));
    assert!(!p1.updated_at.is_empty());
    // camelCase wire format for the desktop readers.
    let raw = fs::read_to_string(wiki::wiki_dir(&pdir).join("current.json")).unwrap();
    assert!(raw.contains("\"updatedAt\""), "camelCase keys: {raw}");
    // Flip again: the SAME pointer file is renamed over, bytes fully new.
    publish(&pdir, &ws, "TOPIC-2.md", "乙", "第二版，依据 `docs/guide.md:L1-L3`。\n");
    let p2 = wiki::current_pointer(&pdir).unwrap();
    assert_eq!((p2.version, p2.build, p2.kind.as_str()), (2, 2, "build"));
    let raw = fs::read_to_string(wiki::wiki_dir(&pdir).join("current.json")).unwrap();
    assert!(!raw.contains("first"), "no residue of the old pointer: {raw}");
    // No temp-file litter anywhere under wiki/ after two real flips.
    fn assert_no_tmp(dir: &Path) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                assert_no_tmp(&p);
            } else {
                let n = e.file_name().to_string_lossy().into_owned();
                assert!(!n.ends_with(".tmp"), "temp residue after commit: {n}");
            }
        }
    }
    assert_no_tmp(&wiki::wiki_dir(&pdir));
    let _ = t;
}

// ------------------------------------------------------------------ legacy

/// The pre-versioning layout is version 0: reads fall back to it, the first
/// commit carries its pages forward, and its files are never rewritten.
#[test]
fn legacy_compiled_dir_reads_as_version_zero_and_upgrades_on_first_commit() {
    let (_t, _data, ws, pdir) = setup();
    let legacy = wiki::legacy_compiled_dir(&pdir);
    fs::create_dir_all(&legacy).unwrap();
    let extra = "# 蒸馏知识\n\n会话审核入库的页面。\n";
    fs::write(legacy.join("USER.md"), extra).unwrap();

    // Read fallback BEFORE any pointer exists.
    assert!(wiki::current_pointer(&pdir).is_none());
    assert_eq!(wiki::compiled_dir(&pdir), legacy);
    assert_eq!(mds(&wiki::compiled_dir(&pdir)), vec!["USER.md"]);

    publish(&pdir, &ws, "TOPIC-1.md", "甲", "第一版，依据 `docs/guide.md:L1-L3`。\n");
    let p = wiki::current_pointer(&pdir).expect("pointer after first commit");
    assert_eq!(p.version, 1);
    let v1 = wiki::versions_root(&pdir).join("1");
    assert_eq!(wiki::compiled_dir(&pdir), v1);
    assert_eq!(mds(&v1), ["TOPIC-1.md", "USER.md"]);
    assert_eq!(fs::read_to_string(v1.join("USER.md")).unwrap(), extra, "carried byte-for-byte");
    // The legacy dir keeps its bytes and is NEVER written again: no article
    // of the new build appears there.
    assert_eq!(mds(&legacy), vec!["USER.md"]);

    publish(&pdir, &ws, "TOPIC-2.md", "乙", "第二版，依据 `docs/guide.md:L1-L3`。\n");
    // The carry-forward now chains from v1, not the legacy dir.
    assert_eq!(mds(&wiki::versions_root(&pdir).join("2")), ["TOPIC-1.md", "TOPIC-2.md", "USER.md"]);
    assert_eq!(mds(&legacy), vec!["USER.md"], "legacy stays frozen forever");
}

// ------------------------------------------------------------ carry-forward

#[test]
fn user_edited_and_foreign_pages_carry_into_every_new_version() {
    let (_t, _data, ws, pdir) = setup();
    publish(&pdir, &ws, "TOPIC-1.md", "甲", "机器第一版，依据 `docs/guide.md:L1-L3`。\n");

    // A human edits one article in the CURRENT version, and a distilled
    // knowledge page lives beside it (never written by any build flow).
    let human = "人工修订内容，模型不许覆盖。\n";
    fs::write(wiki::compiled_dir(&pdir).join("TOPIC-1.md"), human).unwrap();
    assert!(wb::mark_user_edited(&pdir, "TOPIC-1.md"));
    let distilled = "# 会话沉淀\n\n经人工审核入库。\n";
    fs::write(wiki::compiled_dir(&pdir).join("KNOWLEDGE.md"), distilled).unwrap();

    // A later build publishes a different page only.
    publish(&pdir, &ws, "TOPIC-2.md", "乙", "机器新版乙，依据 `docs/guide.md:L1-L3`。\n");
    let v2 = wiki::compiled_dir(&pdir);
    assert_eq!(wiki::current_pointer(&pdir).unwrap().version, 2);
    assert_eq!(fs::read_to_string(v2.join("TOPIC-1.md")).unwrap(), human);
    assert_eq!(fs::read_to_string(v2.join("KNOWLEDGE.md")).unwrap(), distilled);
    assert!(fs::read_to_string(v2.join("TOPIC-2.md")).unwrap().contains("机器新版乙"));
}

// -------------------------------------------------------------------- gc

#[test]
fn gc_keeps_the_newest_k_and_never_the_current_version() {
    let (_t, _data, ws, pdir) = setup();
    for i in 1..=3u64 {
        publish(&pdir, &ws, &format!("TOPIC-{i}.md"), "题", &format!("第{i}版，依据 `docs/guide.md:L1-L3`。\n"));
    }
    // Each publish already GCs with the default keep (3): never below the
    // current, never above K+current. Sweep explicitly at K=2 — the argument
    // form keeps this test independent of the process-global knob.
    wiki::gc_versions(&pdir, 2);
    assert_eq!(version_dirs(&pdir), vec![2, 3]);
    // Everything still reads, current included (carry-forward keeps pages).
    let cur = wiki::current_pointer(&pdir).unwrap().version;
    assert_eq!(cur, 3);
    assert_eq!(mds(&wiki::compiled_dir(&pdir)), ["TOPIC-1.md", "TOPIC-2.md", "TOPIC-3.md"]);
    // K=1 can never evict the CURRENT version: keep=1 keeps exactly {3}.
    wiki::gc_versions(&pdir, 1);
    assert_eq!(version_dirs(&pdir), vec![3]);
    assert!(wiki::compiled_dir(&pdir).is_dir());
}

// ----------------------------------------------------- real-kill recovery

fn spawn_parked(test_name: &str, stage: &str, data: &Path, ws: &Path, marker: &Path) -> std::process::Child {
    let exe = std::env::current_exe().unwrap();
    std::process::Command::new(exe)
        .arg(test_name)
        .arg("--exact")
        .arg("--ignored")
        .arg("--nocapture")
        .env("DANDELION_WIKI_TEST_PARK", stage)
        .env("DANDELION_WIKI_TEST_PARK_MARKER", marker)
        .env("G9_DATA", data)
        .env("G9_WS", ws)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn parked child")
}

fn wait_for_marker(marker: &Path) {
    for _ in 0..600 {
        if marker.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("child never reached the park point (30s)");
}

fn kill_and_reap(mut child: std::process::Child) {
    child.kill().expect("TerminateProcess");
    child.wait().expect("reap");
}

fn child_env() -> (PathBuf, PathBuf) {
    (
        PathBuf::from(std::env::var("G9_DATA").expect("child env")),
        PathBuf::from(std::env::var("G9_WS").expect("child env")),
    )
}

/// Child: publishes the SECOND article; the production publish path parks
/// inside the window named by `DANDELION_WIKI_TEST_PARK`.
#[test]
#[ignore = "g9 spawns this as a parked child process for kill tests"]
fn g9_child_publish() {
    let (data, ws) = child_env();
    let pdir = resolved(&data, &ws);
    let index = wi::build_index(&pdir, &ws, 100, 65_536).unwrap();
    wb::publish_build(
        &pdir,
        &index,
        &[art("TOPIC-2.md", "乙", "第二版内容，依据 `docs/guide.md:L1-L3`。\n")],
    )
    .unwrap();
}

/// Child: runs a full scan; parks right before its pointer flip.
#[test]
#[ignore = "g9 spawns this as a parked child process for kill tests"]
fn g9_child_scan() {
    let (data, ws) = child_env();
    let pdir = resolved(&data, &ws);
    wiki::compile_with_budget(&pdir, &ws, 100, 65_536, None).unwrap();
}

/// Kill BETWEEN the ledger writes and the pointer flip: the visible Wiki is
/// untouched the whole time, and recovery rewinds the ledgers and removes
/// the orphan snapshot — no handcrafted journal involved.
#[test]
fn kill_between_ledgers_and_commit_reads_old_version_and_recovers_clean() {
    let (t, data, ws, pdir) = setup();
    publish(&pdir, &ws, "TOPIC-1.md", "甲", "第一版，依据 `docs/guide.md:L1-L3`。\n");
    let v1 = wiki::compiled_dir(&pdir);
    let v1_num = wiki::current_pointer(&pdir).unwrap().version;
    let v1_bytes = fs::read(v1.join("TOPIC-1.md")).unwrap();

    let marker = t.path().join("marker1");
    let child = spawn_parked("g9_child_publish", "publish-before-switch", &data, &ws, &marker);
    wait_for_marker(&marker);
    kill_and_reap(child);

    // While the corpse cools: readers still see ONLY the old version.
    assert_eq!(wiki::current_pointer(&pdir).unwrap().version, v1_num);
    assert_eq!(wiki::compiled_dir(&pdir), v1);
    assert_eq!(fs::read(v1.join("TOPIC-1.md")).unwrap(), v1_bytes);
    assert!(!wiki::compiled_dir(&pdir).join("TOPIC-2.md").exists(), "never visible");
    // Ledgers are ahead (dirty), the journal stands, the orphan waits.
    assert_eq!(wb::load_articles(&pdir).version, 2, "ledger written pre-commit");
    assert!(wb::journal_path(&pdir).exists());
    assert!(wiki::versions_root(&pdir).join((v1_num + 1).to_string()).is_dir());

    let rewound = wb::recover_publish(&pdir).unwrap();
    assert!(!rewound.is_empty(), "the journaled ledgers were rewound: {rewound:?}");
    assert_eq!(wb::load_articles(&pdir).version, 1);
    assert!(!wb::journal_path(&pdir).exists());
    assert!(
        !wiki::versions_root(&pdir).join((v1_num + 1).to_string()).is_dir(),
        "the orphan snapshot was removed whole"
    );
    assert_eq!(wiki::current_pointer(&pdir).unwrap().version, v1_num);
    assert_eq!(fs::read(v1.join("TOPIC-1.md")).unwrap(), v1_bytes, "old version byte-intact");
}

/// Kill AFTER the pointer flip but before journal cleanup: recovery must
/// RECOGNISE the commit (via target_version) and finish the housekeeping
/// instead of unwinding a completed build.
#[test]
fn kill_after_commit_is_finished_off_as_committed_not_rewound() {
    let (t, data, ws, pdir) = setup();
    publish(&pdir, &ws, "TOPIC-1.md", "甲", "第一版，依据 `docs/guide.md:L1-L3`。\n");

    let marker = t.path().join("marker2");
    let child = spawn_parked("g9_child_publish", "publish-after-commit", &data, &ws, &marker);
    wait_for_marker(&marker);
    kill_and_reap(child);

    // The new version is ALREADY the visible Wiki at the kill point.
    let p = wiki::current_pointer(&pdir).unwrap();
    assert_eq!((p.version, p.build), (2, 2));
    assert!(wiki::compiled_dir(&pdir).join("TOPIC-2.md").is_file());
    // The journal survived the kill (cleanup never ran).
    assert!(wb::journal_path(&pdir).exists(), "kill landed before journal cleanup");

    let rewound = wb::recover_publish(&pdir).unwrap();
    assert!(rewound.is_empty(), "commit recognised — nothing rewound: {rewound:?}");
    assert!(!wb::journal_path(&pdir).exists(), "journal housekeeping finished");
    let p = wiki::current_pointer(&pdir).unwrap();
    assert_eq!((p.version, p.build), (2, 2), "committed version stands");
    assert_eq!(wb::load_articles(&pdir).version, 2);
    let cur = wiki::compiled_dir(&pdir);
    assert!(fs::read_to_string(cur.join("TOPIC-1.md")).unwrap().contains("第一版"));
    assert!(fs::read_to_string(cur.join("TOPIC-2.md")).unwrap().contains("第二版内容"));
}

/// Kill inside a SCAN, between its meta write and its pointer flip: the old
/// version never blinks, the orphan is invisible and sweepable.
#[test]
fn kill_before_scan_switch_leaves_reads_untouched_and_orphan_sweepable() {
    let (t, data, ws, pdir) = setup();
    publish(&pdir, &ws, "TOPIC-1.md", "甲", "第一版，依据 `docs/guide.md:L1-L3`。\n");
    let v1_num = wiki::current_pointer(&pdir).unwrap().version;
    let v1 = wiki::compiled_dir(&pdir);
    let v1_names = mds(&v1);
    let v1_bytes = fs::read(v1.join("TOPIC-1.md")).unwrap();

    let marker = t.path().join("marker3");
    let child = spawn_parked("g9_child_scan", "scan-before-switch", &data, &ws, &marker);
    wait_for_marker(&marker);
    kill_and_reap(child);

    // Every read resolves to the untouched old version.
    assert_eq!(wiki::current_pointer(&pdir).unwrap().version, v1_num);
    assert_eq!(wiki::compiled_dir(&pdir), v1);
    assert_eq!(mds(&v1), v1_names);
    assert_eq!(fs::read(v1.join("TOPIC-1.md")).unwrap(), v1_bytes);
    // The orphan snapshot exists but names nothing any reader resolves to.
    let orphan = wiki::versions_root(&pdir).join((v1_num + 1).to_string());
    assert!(orphan.is_dir(), "the scan's snapshot was in flight");
    // Real rotation sweeps it: the NEXT commit numbers itself PAST the orphan
    // (next_snapshot_version = max(pointer, dirs) + 1 — numbers never collide
    // with orphans), and once the orphan falls out of the keep window the
    // ordinary GC removes it.
    publish(&pdir, &ws, "TOPIC-2.md", "乙", "第二版，依据 `docs/guide.md:L1-L3`。\n");
    let p = wiki::current_pointer(&pdir).unwrap();
    assert_eq!(p.version, v1_num + 2, "new snapshot skips the orphan's number");
    wiki::gc_versions(&pdir, 1);
    assert_eq!(version_dirs(&pdir), vec![p.version], "rotation retires the orphan");
    assert!(fs::read_to_string(wiki::compiled_dir(&pdir).join("TOPIC-1.md")).unwrap().contains("第一版"));
}
