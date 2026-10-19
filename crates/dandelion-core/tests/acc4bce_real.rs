//! ACCEPTANCE PROBE — work order 4bce real-model acceptance (temporary file).
//!
//! Every test here is `#[ignore]`d: it talks to the REAL configured model and
//! (where a workspace is involved) REAL data, copied into an isolated data
//! dir so the user's live `dandelion` state is never mutated. REA
//! (`E:\ai-open-resource\rea`) is read strictly read-only: the build runs on
//! a copy and the original's `git status --porcelain` is checked afterwards.
//!
//! Run (one scenario at a time, evidence lands under DLN_ACC_OUT):
//!   cargo test -p dandelion-core --test acc4bce_real acc01 -- --ignored --nocapture --test-threads=1
//!
//! Env: DLN_ACC_DATA (real data dir, default %APPDATA%\dandelion\dandelion\data)
//!      DLN_ACC_OUT (evidence dir, default %TEMP%\dln-acc-4bce)
//!      DLN_ACC_REA (REA checkout, default E:\ai-open-resource\rea)
//!
//! API key values are never printed — evidence records model *answers*, the
//! harness reads the key only through `Settings::load` on the isolated copy.

use dandelion_core::host::CliHost;
use dandelion_core::store::sqlite::Store;
use dandelion_core::wiki::{self, WikiProfile, WikiSubmission};
use dandelion_core::wiki_build;
use dandelion_core::wiki_space_migrate::MigrationReport;
use dandelion_core::{Runtime, SettingsOverlay};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SUBJECT: &str = "local-user";

// ---------------------------------------------------------------- environment

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(default))
}

fn real_data() -> PathBuf {
    let appdata = std::env::var("APPDATA").expect("APPDATA");
    env_path("DLN_ACC_DATA", &format!("{appdata}\\dandelion\\dandelion\\data"))
}

fn out_root() -> PathBuf {
    let p = env_path("DLN_ACC_OUT", {
        let t = std::env::var("TEMP").expect("TEMP");
        &format!("{t}\\dln-acc-4bce")
    });
    fs::create_dir_all(&p).unwrap();
    p
}

fn rea_path() -> PathBuf {
    env_path("DLN_ACC_REA", "E:\\ai-open-resource\\rea")
}

/// Evidence log: every assertion-relevant fact is appended to a durable file
/// (the deliverable evidence trail), not just to the test output.
fn evidence(name: &str) -> PathBuf {
    let dir = out_root().join("evidence");
    fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}.log"))
}

fn log(name: &str, line: &str) {
    let path = evidence(name);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
    eprintln!("[{name}] {line}");
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn copy_dir_skip(src: &Path, dst: &Path, skip: &[&str]) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if skip.contains(&name.as_str()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_dir_skip(&from, &to, skip);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

/// Isolated data dir: a faithful copy of the real settings (so the REAL
/// provider/key is used) plus whatever live state the scenario names.
/// The real dir is only ever READ.
fn isolate(tag: &str, include_live_wiki: bool, include_db: bool) -> PathBuf {
    let real = real_data();
    let dir = out_root().join(format!("data-{tag}"));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::copy(real.join("settings.json"), dir.join("settings.json")).unwrap();
    if real.join("secrets").is_dir() {
        copy_dir(&real.join("secrets"), &dir.join("secrets"));
    }
    if include_db {
        for name in ["sessions.sqlite", "sessions.sqlite-wal", "sessions.sqlite-shm"] {
            if real.join(name).exists() {
                fs::copy(real.join(name), dir.join(name)).unwrap();
            }
        }
    }
    if include_live_wiki && real.join("wiki").is_dir() {
        copy_dir(&real.join("wiki"), &dir.join("wiki"));
    }
    dir
}

fn boot(data: &Path) -> Arc<Runtime> {
    Arc::new(
        Runtime::new_with_overlay(
            Arc::new(CliHost::with_data_dir(data.to_path_buf())),
            SettingsOverlay::default(),
        )
        .expect("runtime over the real (copied) settings must construct"),
    )
}

fn store(data: &Path) -> Store {
    Store::open(data, 5_000).unwrap()
}

// ------------------------------------------------- per-token citation checker

/// verify3-style checker: EVERY `path:Lx[-Ly]` citation in every published
/// article must resolve under `root`, the file must exist, and the line span
/// must fit the file's actual line count. URL citations are locatable but not
/// line-verifiable (counted, honestly labelled). Returns (ok, failures).
fn check_citations(name: &str, bodies: &[(String, String)], root: &Path) -> (usize, Vec<String>) {
    let mut ok = 0usize;
    let mut bad: Vec<String> = Vec::new();
    let mut urls = 0usize;
    for (page, body) in bodies {
        let bytes = body.as_bytes();
        let chars: Vec<char> = body.chars().collect();
        let _ = bytes;
        let mut i = 0usize;
        while i + 1 < chars.len() {
            // Locate ":L" + digit, then walk back to the start of the path.
            if chars[i] == ':' && chars[i + 1] == 'L' && chars[i + 2].is_ascii_digit() {
                let mut j = i;
                while j > 0 {
                    let c = chars[j - 1];
                    if c.is_whitespace()
                        || matches!(c, '`' | '(' | '（' | '[' | '【' | '、' | '，' | '。' | '；' | ';' | '：' | '|' | '*' | '“' | '”')
                    {
                        break;
                    }
                    j -= 1;
                }
                let path: String = chars[j..i].iter().collect();
                let mut k = i + 2;
                let mut start_str = String::new();
                while k < chars.len() && chars[k].is_ascii_digit() {
                    start_str.push(chars[k]);
                    k += 1;
                }
                let mut end = None;
                if k + 1 < chars.len() && chars[k] == '-' && chars[k + 1] == 'L' {
                    let mut e = String::new();
                    k += 2;
                    while k < chars.len() && chars[k].is_ascii_digit() {
                        e.push(chars[k]);
                        k += 1;
                    }
                    end = e.parse::<usize>().ok();
                }
                let line: usize = start_str.parse().unwrap_or(0);
                let clean = path.trim_matches(|c: char| {
                    matches!(c, '<' | '>' | '《' | '》' | '「' | '」' | '\'' | '"')
                });
                if clean.is_empty() || line == 0 {
                    i += 2;
                    continue;
                }
                let rel = clean.replace('\\', "/");
                let mut file = root.join(&rel);
                if !file.exists() {
                    // tolerate basename-only citations
                    if let Some(base) = rel.rsplit('/').next() {
                        file = find_file(root, base).unwrap_or_else(|| root.join(&rel));
                    }
                }
                if !file.is_file() {
                    bad.push(format!("{page}: 引用 {clean}:L{line} — 文件不存在"));
                } else {
                    let lines = fs::read_to_string(&file)
                        .map(|s| s.lines().count())
                        .unwrap_or(usize::MAX);
                    let hi = end.unwrap_or(line);
                    if line <= lines && hi <= lines && hi >= line {
                        ok += 1;
                    } else {
                        bad.push(format!(
                            "{page}: 引用 {clean}:L{line}-L{hi} — 越界（文件仅 {lines} 行）"
                        ));
                    }
                }
                i = k;
                continue;
            }
            if chars[i] == 'h'
                && chars[i..].iter().take(8).collect::<String>().as_str() == "https://"
            {
                urls += 1;
            }
            i += 1;
        }
    }
    log(
        name,
        &format!("引用校验：行级通过 {ok}，URL（可定位不逐行）{urls}，失败 {}", bad.len()),
    );
    for b in &bad {
        log(name, &format!("  引用失败：{b}"));
    }
    (ok, bad)
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
            }
        } else if e.file_name().to_string_lossy() == name {
            return Some(p);
        }
    }
    None
}

/// Minimal hand-built PDF (uncompressed Helvetica, ASCII): the repo ships no
/// PDF fixture, so the acceptance PDF is built here, honestly labelled ASCII.
fn minimal_pdf(text: &str) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objects.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into());
    objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".into());
    let stream = format!("BT /F1 12 Tf 72 720 Td 14 TL\n{}\nET",
        text.lines()
            .map(|l| format!("({}) Tj T*", l.replace('\\', r"\\").replace('(', r"\(").replace(')', r"\)")))
            .collect::<Vec<_>>()
            .join("\n"));
    objects.push(format!("<< /Length {} >>\nstream\n{}\nendstream", stream.len(), stream));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());

    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{}\nendobj\n", i + 1, obj).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend(b"0000000000 65535 f \n");
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1)
            .as_bytes(),
    );
    out
}

fn git_porcelain(repo: &Path) -> String {
    String::from_utf8_lossy(
        &std::process::Command::new("git")
            .args(["-C"])
            .arg(repo)
            .args(["status", "--porcelain"])
            .output()
            .expect("git available")
            .stdout,
    )
    .trim()
    .to_string()
}

fn space_of(data: &Path, ws: &Path, name: &str) -> String {
    let id = store(data)
        .knowledge_space_for_workspace(&wiki::workspace_hash(ws))
        .unwrap()
        .unwrap_or_else(|| panic!("{name}: workspace never resolved to a space"));
    id
}

fn pages_of(rt: &Runtime, space: &str) -> Vec<(String, String)> {
    rt.space_show(SUBJECT, space, None)
        .unwrap()
        .into_iter()
        .map(|p| (p.name.clone(), p.body.clone()))
        .collect()
}

// ============================================================== scenarios

/// acc01 — migration 对账 on an isolated copy of the REAL data dir.
/// Deterministic (no model). Pins: registry resolves on the real spaces, the
/// standing anomaly survives as evidence, and after TWO boots the audit
/// trail holds each finding exactly once (the fd56d9a dedupe). Also records
/// (read-only) the pre-fix duplicate history in the live dir's own report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-data acceptance probe (see module docs)"]
async fn acc01_migration_reconciliation_on_real_copy() {
    let name = "acc01-migration";
    let tag = "acc01";
    let data = isolate(tag, true, true);
    let _ = boot(&data); // boot 1: startup rebuild + report write
    let _ = boot(&data); // boot 2: a standing finding must NOT stack

    let rep: MigrationReport = serde_json::from_slice(
        &fs::read(wiki::wiki_dir(&data).join("space-migration-report.json"))
            .expect("report written on the copy"),
    )
    .unwrap();
    for a in &rep.anomalies {
        log(name, &format!("异常：{a}"));
    }
    for r in &rep.registry_restored {
        log(name, &format!("注册表恢复：{r}"));
    }
    let mut seen: Vec<&String> = Vec::new();
    for a in &rep.anomalies {
        assert!(!seen.contains(&a), "standing anomaly stacked: {a}");
        seen.push(a);
    }
    let missing: Vec<&String> = rep.anomalies.iter().filter(|a| a.contains("origin.json")).collect();
    assert_eq!(
        missing.len(),
        1,
        "the one real missing-origin anomaly must appear exactly once after two boots: {:?}",
        rep.anomalies
    );

    // Registry intact for the space that has its anchors: resolution works.
    let spaces_root = wiki::spaces_dir(&data);
    let mut resolved = 0;
    for e in fs::read_dir(&spaces_root).unwrap().flatten() {
        let dir_name = e.file_name().to_string_lossy().to_string();
        let origin = fs::read(e.path().join(wiki::SPACE_ORIGIN_FILE));
        log(name, &format!("空间 {dir_name}：origin.json {}", if origin.is_ok() { "存在" } else { "缺失（已记异常）" }));
        if origin.is_ok() {
            resolved += 1;
        }
    }
    assert!(resolved >= 1, "at least one anchored space must survive the copy");

    // Read-only record of the LIVE report's pre-fix duplicate history.
    let live = wiki::wiki_dir(&real_data()).join("space-migration-report.json");
    if let Ok(raw) = fs::read(&live) {
        let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let arr = v["anomalies"].as_array().cloned().unwrap_or_default();
        log(name, &format!("（真机原报告，只读）异常共 {} 条", arr.len()));
        let mut counts = std::collections::BTreeMap::new();
        for a in &arr {
            *counts.entry(a.as_str().unwrap_or("?")).or_insert(0usize) += 1;
        }
        for (k, c) in counts {
            log(name, &format!("  ×{c}：{k}"));
        }
    }
    log(name, "acc01 PASS");
}

/// acc02 — permissions & external-model policy, REAL model assertions.
/// ghost/no-member denial, archived denial, local_only degradation note,
/// mixed-pack degradation, and a genuine `cloud_summary` answer from the
/// real model on an `allow_summary` space.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-model acceptance probe (see module docs)"]
async fn acc02_policy_and_authz_real_model() {
    let name = "acc02-policy";
    let data = isolate("acc02", false, false);
    let rt = boot(&data);

    let cloud = store(&data)
        .knowledge_create_local_space(SUBJECT, "云端许可验收空间", "allow_summary")
        .unwrap();
    let local = store(&data)
        .knowledge_create_local_space(SUBJECT, "仅本机验收空间", "local_only")
        .unwrap();
    for (sp, fact) in [(&cloud, "保压阶段的压力上限是 42 兆帕，超时阈值为 3.5 秒。"), (&local, "合模阶段的冷却回路流量下限为每分钟 18 升。")] {
        let (doc, ver) = rt
            .knowledge_import_document("local", "local", &sp.id, SUBJECT, "note", "acc02", fact, 500)
            .unwrap();
        rt.knowledge_publish_version("local", "local", &sp.id, SUBJECT, &doc.id, &ver.id)
            .unwrap();
    }

    // 1) ghost (no membership row) is denied and nothing is echoed back.
    let ghost = rt.space_status("幽灵用户", &cloud.id);
    let err = ghost.unwrap_err().to_string();
    log(name, &format!("无权限读取被拒：{err}"));
    assert!(err.contains("denied") || err.contains("拒绝"), "ghost must be denied: {err}");
    assert!(!err.contains("42 兆帕"), "denial must not echo content");

    // 2) archived space: named refusal.
    store(&data).knowledge_archive_space(&local.id, true).unwrap();
    let arch = rt
        .space_search(SUBJECT, &[local.id.clone()], "冷却", 5)
        .unwrap_err()
        .to_string();
    log(name, &format!("已归档被拒：{arch}"));
    assert!(arch.contains("已归档"), "archived refusal must be named: {arch}");
    store(&data).knowledge_archive_space(&local.id, false).unwrap();

    // 3) local_only + allow_cloud=true → honest degradation, note shown.
    let a = rt
        .space_ask(SUBJECT, std::slice::from_ref(&local.id), "冷却回路的流量下限是多少？", 6, true)
        .await
        .unwrap();
    log(name, &format!("仅本机空间 mode={} note={:?}", a.mode, a.policy_note));
    assert_eq!(a.mode, "local_extract", "local_only must never take the cloud path");
    assert!(a.policy_note.is_some(), "degradation must be stated, not silent");
    assert!(a.answer.contains("18"), "extractive answer must carry the fact: {}", a.answer);

    // 4) mixed pack (allow_summary + local_only) → whole pack degrades.
    let a = rt
        .space_ask(
            SUBJECT,
            &[cloud.id.clone(), local.id.clone()],
            "压力上限是多少？",
            6,
            true,
        )
        .await
        .unwrap();
    log(name, &format!("混合包 mode={} note={:?}", a.mode, a.policy_note));
    assert_eq!(a.mode, "local_extract", "one local_only space must degrade the whole pack");
    assert!(a.policy_note.is_some());

    // 5) REAL MODEL: allow_summary + allow_cloud → cloud_summary answer.
    let a = rt
        .space_ask(SUBJECT, std::slice::from_ref(&cloud.id), "保压阶段的压力上限和超时阈值分别是多少？", 6, true)
        .await
        .unwrap();
    log(name, &format!("云端许可空间 mode={} note={:?}", a.mode, a.policy_note));
    log(name, &format!("云端回答：{}", a.answer.replace('\n', " ")));
    for s in &a.sources {
        log(name, &format!("  出处 {}：{}", s.label, s.excerpt.replace('\n', " ")));
    }
    assert_eq!(a.mode, "cloud_summary", "policy-permitted cloud ask must actually run the model");
    assert!(!a.no_answer && a.answer.contains("42"), "real-model answer must carry the fact");
    log(name, "acc02 PASS");
}

/// acc03 — business space (md + PDF + web page + conversation) built with
/// the REAL model, per-token citation check, then unified search & ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-model acceptance probe (see module docs)"]
async fn acc03_business_space_mixed_build_and_qa() {
    let name = "acc03-business";
    let data = isolate("acc03", false, false);
    let docs = out_root().join("docs-acc03");
    if docs.exists() {
        fs::remove_dir_all(&docs).unwrap();
    }
    fs::create_dir_all(docs.join("网页")).unwrap();
    fs::create_dir_all(docs.join("对话记录")).unwrap();
    fs::write(
        docs.join("产品说明.md"),
        "# 注塑成型作业说明\n\n保压阶段的压力上限是 42 兆帕，超时阈值为 3.5 秒。\n合模阶段的冷却回路流量下限为每分钟 18 升，低于该值报警。\n换料清机必须执行三次空打，每次空打料筒温度不得低于 180 摄氏度。\n",
    )
    .unwrap();
    fs::write(
        docs.join("网页/公告.html"),
        "<html><body><h1>设备点检公告</h1><p>每日开机后须先执行 5 分钟空运转点检，液压油温须达到 25 摄氏度方可加载。</p></body></html>",
    )
    .unwrap();
    fs::write(
        docs.join("对话记录/班后会话.md"),
        "# 班后会话\n问：夜班最常见的问题是什么？\n答：冷却流量波动，多为过滤器堵塞，建议每班清洗。\n问：过滤器多久更换？\n答：累计运行 500 小时强制更换。\n",
    )
    .unwrap();
    fs::write(
        docs.join("安全须知.pdf"),
        minimal_pdf("SAFETY NOTICE: interlock must stay enabled.\nMaximum clamp pressure is 42 MPa.\nOperator must complete the 5 minute warm-up."),
    )
    .unwrap();

    let rt = boot(&data);
    let report = rt
        .wiki_build_llm(Some(&docs), false)
        .await
        .expect("real-model mixed-media build must succeed");
    log(
        name,
        &format!("BuildReport published={} incremental={:?} warnings={:?}", report.published.len(), report.incremental, report.warnings),
    );

    let space = space_of(&data, &docs, "acc03");
    log(name, &format!("业务空间 id={space}（本地空间，kind=wiki）"));
    let status = rt.space_status(SUBJECT, &space).unwrap();
    let pages = pages_of(&rt, &space);
    assert!(!status.pages.is_empty() && pages.len() >= 2, "mixed sources must yield articles");
    for (p, body) in &pages {
        log(name, &format!("文章《{p}》 {} 字节", body.len()));
        fs::write(out_root().join(format!("acc03-{p}.md")), body).unwrap();
    }
    let (ok, bad) = check_citations(name, &pages, &docs);
    assert!(ok > 0, "articles must carry resolvable line citations");
    assert!(bad.is_empty(), "per-token citation check found failures: {bad:?}");

    let hits = rt.space_search(SUBJECT, std::slice::from_ref(&space), "冷却流量下限", 5).unwrap();
    log(name, &format!("检索命中 {} 条", hits.hits.len()));
    for h in &hits.hits {
        log(name, &format!("  命中 [{}] {}", h.surface, h.label));
    }
    assert!(!hits.hits.is_empty(), "unified search must hit the built space");

    let a = rt
        .space_ask(SUBJECT, std::slice::from_ref(&space), "保压阶段的压力上限是多少？", 6, false)
        .await
        .unwrap();
    log(name, &format!("问答 mode={} note={:?}", a.mode, a.policy_note));
    log(name, &format!("回答：{}", a.answer.replace('\n', " ")));
    assert!(a.answer.contains("42"), "answer must carry the fact: {}", a.answer);
    assert!(!a.sources.is_empty(), "answer must carry sources");
    log(name, "acc03 PASS");
}

/// acc04 — REA minus README, forced `generic` profile, REAL-model deep build
/// on a COPY (original verified git-clean after), per-token citation check,
/// then REAL-MODEL mid-build cancel: a second concurrent build is refused by
/// the lock, the in-flight build is cancelled, and the previously published
/// articles stay readable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-model acceptance probe (see module docs)"]
async fn acc04_rea_generic_build_then_cancel_keeps_old_readable() {
    let name = "acc04-rea";
    let rea = rea_path();
    assert_eq!(git_porcelain(&rea), "", "REA original must be clean BEFORE the probe");

    let copy = out_root().join("rea-copy");
    if copy.exists() {
        fs::remove_dir_all(&copy).unwrap();
    }
    copy_dir_skip(&rea, &copy, &[".git"]);
    fs::remove_file(copy.join("README.md")).ok();
    log(name, &format!("REA 副本就绪（去 README、去 .git）：{}", copy.display()));

    let data = isolate("acc04", false, false);
    let rt = boot(&data);
    rt.wiki_set_profile_for(Some(&copy), &WikiProfile::generic())
        .expect("generic profile must be settable");

    let report = rt
        .wiki_build_llm(Some(&copy), false)
        .await
        .expect("real-model generic build over REA copy must succeed");
    log(name, &format!("BuildReport published={} warnings={:?}", report.published.len(), report.warnings));

    let space = space_of(&data, &copy, "acc04");
    let pages = pages_of(&rt, &space);
    assert!(!pages.is_empty(), "generic build must produce articles");
    let mut all = String::new();
    for (p, body) in &pages {
        all.push_str(body);
        log(name, &format!("文章《{p}》 {} 字节", body.len()));
        let dump = if p.ends_with(".md") { format!("acc04-{p}") } else { format!("acc04-{p}.md") };
        fs::write(out_root().join(dump), body).unwrap();
    }
    let (ok, bad) = check_citations(name, &pages, &copy);
    assert!(ok > 0, "generic articles must carry resolvable citations");
    assert!(bad.is_empty(), "per-token citation failures: {bad:?}");
    // Work-order requirement for THIS scenario: generic 降级 must still
    // produce 结构 + 实现流程. The plan-level generic floor
    // (主题地图/内容结构/脉络流程/阅读指引) is pinned by g15 and, by design,
    // only tops topics up to the article budget — it never guarantees the
    // four words appear in prose, and this corpus is English (first run:
    // the tree legitimately came out bilingual, floor topics merged into
    // model-chosen titles). What must survive regardless of language is
    // the two work-order concepts, checked case-insensitively bilingual.
    let lower = all.to_lowercase();
    let has = |words: [&str; 3]| words.iter().any(|w| lower.contains(w));
    assert!(has(["结构", "structure", "architecture"]), "generic build must surface the corpus structure");
    assert!(has(["流程", "flow", "process"]), "generic build must surface the implementation flow");

    // --- cancel + conflict on the SAME workspace (rebuild attempt) ---------
    let rt2 = Arc::clone(&rt);
    let build = tokio::spawn(async move { rt2.wiki_build_llm(Some(&copy_clone()), false).await });
    // wait for the build slot, then fire the conflicting second build
    while !wiki_build::is_building(&rt.wiki_dir_for(Some(&copy))) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let conflict = rt.wiki_build_llm(Some(&copy), false).await;
    let msg = conflict.unwrap_err().to_string();
    log(name, &format!("并发第二构建被拒：{msg}"));
    assert!(msg.contains("构建") || msg.contains("进行中"), "conflict must name the running build: {msg}");

    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let asked = wiki_build::request_cancel(&rt.wiki_dir_for(Some(&copy)));
    log(name, &format!("request_cancel 返回 {asked}（构建须在场）"));
    assert!(asked, "cancel must attach to the in-flight build");
    let outcome = build.await.unwrap();
    let err = outcome.expect_err("cancelled rebuild must end as an error");
    let msg = err.to_string();
    log(name, &format!("取消结果：{msg}"));
    assert!(msg.contains("取消"), "cancellation must be named: {msg}");
    assert!(!wiki_build::is_building(&rt.wiki_dir_for(Some(&copy))));

    // old published version still readable after the cancelled rebuild
    let pages_after = pages_of(&rt, &space);
    assert_eq!(
        pages_after.len(),
        pages.len(),
        "cancelled build must not damage the published version"
    );
    log(name, "取消后旧版本仍可读 ✓");

    assert_eq!(git_porcelain(&rea), "", "REA original must be untouched AFTER the probe");
    log(name, "acc04 PASS");
}

fn copy_clone() -> PathBuf {
    out_root().join("rea-copy")
}

/// acc05 — empty space: browsing an empty space is an honest empty status,
/// manual ledger pages via `submit_wiki(space_id)` (propose → pending,
/// idempotent duplicate, malformed-id refusal), all under
/// `wiki/spaces/<id>/` with NO project directory involved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-data acceptance probe (needs the copied settings only)"]
async fn acc05_empty_space_manual_pages() {
    let name = "acc05-empty-space";
    let data = isolate("acc05", false, false);
    let rt = boot(&data);
    let sp = store(&data)
        .knowledge_create_local_space(SUBJECT, "空知识空间验收", "local_only")
        .unwrap();

    // 1) empty space: honest empty status, never an error.
    let status = rt.space_status(SUBJECT, &sp.id).unwrap();
    assert!(status.pages.is_empty(), "a fresh space has no published pages yet");
    let shown = rt.space_show(SUBJECT, &sp.id, None).unwrap();
    assert!(shown.is_empty());
    log(name, &format!("空空间状态 OK（{} 页）", status.pages.len()));

    // 2) propose a manual entry straight into the space ledger — no workspace.
    let mk = |key: &str, content: &str| WikiSubmission {
        kind: "term".into(),
        content: content.into(),
        mode: "propose".into(),
        revises: None,
        idempotency_key: Some(key.into()),
        session_id: "acc05".into(),
        turn: Some(1),
        workspace: None,
        space_id: Some(sp.id.clone()),
    };
    let r1 = wiki::submit_wiki(&data, &mk("acc05-k1", "验收术语：蒲公英知识空间以空间 id 为唯一身份，名称与路径只是展示。"), 64 * 1024).unwrap();
    assert_eq!(r1.status(), "pending", "a proposal lands pending: {:?}", r1.status());
    let r2 = wiki::submit_wiki(&data, &mk("acc05-k1", "验收术语：蒲公英知识空间以空间 id 为唯一身份，名称与路径只是展示。"), 64 * 1024).unwrap();
    assert_eq!(r2.status(), "duplicate", "same idempotency key must be a duplicate");
    log(name, &format!("提议 → pending；幂等重放 → duplicate（entry {:?}）", r1.entry_id()));

    // 3) malformed space id is refused with the named shape error — and no
    //    workspace fallback happens.
    let bad = WikiSubmission {
        space_id: Some("坏 id !".into()),
        ..mk("acc05-k2", "任何内容都必须被拒绝，因为空间 id 形状非法。")
    };
    let r3 = wiki::submit_wiki(&data, &bad, 64 * 1024).unwrap();
    assert_eq!(r3.status(), "failed");
    let reason = match &r3 {
        wiki::WikiSubmitResult::Failed { reason } => reason.clone(),
        _ => unreachable!(),
    };
    log(name, &format!("畸形空间 id 被拒：{reason}"));
    assert!(reason.contains("形状非法"), "refusal must name the shape: {reason}");

    // 4) the ledger lives under wiki/spaces/<id>/ and nowhere else.
    let space_dir = wiki::space_data_dir(&data, &sp.id);
    let mut hits = Vec::new();
    collect_files(&space_dir, &mut hits);
    let ledger = hits.iter().find(|p| {
        fs::read_to_string(p).ok().is_some_and(|s| s.contains("以空间 id 为唯一身份"))
    });
    assert!(ledger.is_some(), "pending entry must persist under the space dir {space_dir:?}");
    log(name, &format!("台账落盘：{:?}", ledger.unwrap().strip_prefix(&data).unwrap()));
    assert!(
        !wiki::spaces_dir(&data).join("legacy-leak").exists(),
        "no path outside the space tree may hold the entry"
    );

    // 5) in-app review through the SPACE-keyed entry points (the gap the
    //    first acc05 run recorded; `space_entries`/`space_review_entry`/
    //    `space_edit_entry` closed it). Pure space: no workspace, no
    //    idle-compile queue, no error.
    let entry_id = r1.entry_id().expect("pending entry").to_string();
    let listed = rt.space_entries(SUBJECT, &sp.id).unwrap();
    assert_eq!(listed.iter().filter(|e| e.id == entry_id).count(), 1);
    rt.space_review_entry(SUBJECT, &sp.id, &entry_id, "approve").unwrap();
    assert_eq!(
        rt.space_entries(SUBJECT, &sp.id).unwrap().iter().find(|e| e.id == entry_id).unwrap().status,
        "accepted"
    );
    rt.space_edit_entry(SUBJECT, &sp.id, &entry_id, Some("验收术语：空间 id 是唯一身份；此条经真机审核修订。"), None).unwrap();
    let edited = rt.space_entries(SUBJECT, &sp.id).unwrap().into_iter().find(|e| e.id == entry_id).unwrap();
    assert!(edited.content.contains("真机审核修订"));
    rt.space_review_entry(SUBJECT, &sp.id, &entry_id, "undo").unwrap();
    assert!(rt.space_entries(SUBJECT, &sp.id).unwrap().iter().all(|e| e.id != entry_id));
    log(name, "空间台账审核闭环 OK：list → approve(accepted) → edit → undo(移除)，纯空间无编译队列亦无报错。");
    log(name, "acc05 PASS");
}

const ACC06_DOCS: [(&str, &str); 3] = [
    (
        "quote-policy",
        "外贸报价必须使用统一模板 QT-2024。\n标准报价由销售专员直接发出，无需审批。\n三个点以内的折扣由销售自行决定。\n三个点到五个点之间的折扣须经理审批。\n折扣超过五个点须总监签字。\n所有特价必须当日向商务部备案。\n报价有效期为三十天。\n过期后必须重新出具，不允许口头延期。\n样品按样品价收费的，仍须出具标准合同。\n财务只认可经过审批的报价单所载合同价。\n",
    ),
    (
        "molding-spec",
        "模具温度波动超过五度容易出现缩水。\n保压时间不足会出现表面凹陷。\n原料含水率超标未烘干，产品表面会出现银纹。\nABS 原料须八十五度烘干三小时。\n尼龙原料须九十度烘干四小时。\n周期时间缩短两成属于正常波动。\n冷却水温差超过三度须通报工艺工程师。\n每次换模后先试模三十模，首件检验合格方可批量生产。\n首件检验记录保存一年。\n工艺卡由工艺部维护，产线不得擅自修改。\n",
    ),
    (
        "order-flow",
        "销售合同生效后两小时内由 ERP 生成销售订单。\n订单变更必须书面通知生产和仓库。\n口头变更订单造成的损失由责任销售承担。\n出库必须依据拣货单执行。\n发货前由质检出具出货检验报告。\n报关单据由单证员制作，货代负责报关。\nOA 付款方式项下以信用险保额为限。\n逾期未收款的订单财务部可拒绝放行。\n售后申请由商务部登记，质量问题二十四小时内转售后部。\n客户投诉数据进入月度经营分析会。\n",
    ),
];

/// acc06 — REAL-model deep build of a pure knowledge space from its own
/// published material (the gap-② acceptance): the store holds the docs,
/// the build reads the regenerated one-way mirror under
/// `wiki/spaces/<id>/sources/`, the SAME pipeline publishes into the
/// space's own data dir. Also pins, on the real machine, that a
/// `local_only` space is refused by the space's EXTERNAL POLICY before
/// any model turn (deep build has no extractive fallback).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real-data acceptance probe (needs the copied settings only)"]
async fn acc06_space_deep_build_real_model() {
    let name = "acc06-space-deep-build";
    let data = isolate("acc06", false, false);
    let rt = boot(&data);

    // 0) external policy on the REAL machine: a local_only space must be
    //    refused by the space's own policy, before the mirror exists and
    //    before any model call (deep build degrades to nothing — the
    //    model step IS the build).
    let strict = store(&data)
        .knowledge_create_local_space(SUBJECT, "本机限定策略空间", "local_only")
        .unwrap();
    let (st, sc) = {
        let s = store(&data).knowledge_space_by_id(&strict.id).unwrap().unwrap();
        (s.tenant_id, s.customer_id)
    };
    let (doc, ver) = rt
        .knowledge_import_document(&st, &sc, &strict.id, SUBJECT, "note", "quote-policy", ACC06_DOCS[0].1, 500)
        .unwrap();
    rt.knowledge_publish_version(&st, &sc, &strict.id, SUBJECT, &doc.id, &ver.id).unwrap();
    let err = rt
        .space_build_llm(SUBJECT, &strict.id, false)
        .await
        .expect_err("local_only must never be deep-built with the external model");
    let msg = err.to_string();
    log(name, &format!("local_only 空间深度构建被外部策略拒绝：{msg}"));
    assert!(msg.contains("本机限定策略空间") && msg.contains("外部策略禁止云端模型"), "{msg}");
    assert!(!wiki::space_data_dir(&data, &strict.id).join("sources").exists(),
        "a policy refusal must not materialize the mirror");

    // 1) the acceptance space: policy allows cloud summaries.
    let sp = store(&data)
        .knowledge_create_local_space(SUBJECT, "外贸业务知识库（acc06）", "allow_summary")
        .unwrap();
    let (t, c) = {
        let s = store(&data).knowledge_space_by_id(&sp.id).unwrap().unwrap();
        (s.tenant_id, s.customer_id)
    };
    for (sid, content) in ACC06_DOCS {
        let (doc, ver) = rt
            .knowledge_import_document(&t, &c, &sp.id, SUBJECT, "note", sid, content, 500)
            .unwrap();
        rt.knowledge_publish_version(&t, &c, &sp.id, SUBJECT, &doc.id, &ver.id).unwrap();
    }

    // 2) the mirror is the build's evidence dir, regenerated from the store.
    let (sources, n) = rt.space_materialize_sources(SUBJECT, &sp.id).unwrap();
    assert_eq!(n, 3, "three published docs must materialize");
    let mut mirror: Vec<String> = fs::read_dir(&sources)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    mirror.sort();
    log(name, &format!("镜像落盘 3 份：{mirror:?}"));

    // 3) REAL-model deep build over the space's own material.
    let report = rt
        .space_build_llm(SUBJECT, &sp.id, false)
        .await
        .expect("real-model space deep build must succeed");
    log(name, &format!(
        "BuildReport build={} targets={:?} published={} warnings={:?} model_calls={} elapsed_ms={}",
        report.build, report.targets, report.published.len(), report.warnings,
        report.model_calls, report.elapsed_ms
    ));
    let (_, bound) = store(&data)
        .knowledge_workspace_for_space(&sp.id)
        .unwrap()
        .expect("a built space registers its mirror as build workspace");
    assert_eq!(Path::new(&bound), sources.as_path(), "the binding points at the mirror, nowhere else");

    // 4) every published page is non-empty, dumps to the evidence dir, and
    //    EVERY line-span citation resolves against the mirror (per-token).
    let pages = pages_of(&rt, &sp.id);
    assert!(!pages.is_empty(), "a built space must publish pages");
    for (p, body) in &pages {
        assert!(!body.trim().is_empty(), "published page {p} is empty");
        log(name, &format!("文章《{p}》 {} 字节", body.len()));
        let dump = if p.ends_with(".md") { format!("acc06-{p}") } else { format!("acc06-{p}.md") };
        fs::write(out_root().join(dump), body).unwrap();
    }
    let (ok, bad) = check_citations(name, &pages, &sources);
    assert!(ok > 0, "space articles must carry resolvable citations");
    assert!(bad.is_empty(), "per-token citation failures: {bad:?}");

    // 5) the unified read names the built articles and chunks.
    for q in ["折扣 审批 总监", "缩水 保压 烘干", "发货 质检 拣货单"] {
        let hits = rt.space_search(SUBJECT, std::slice::from_ref(&sp.id), q, 6).unwrap().hits;
        assert!(!hits.is_empty(), "built space must answer {q}");
        let top = &hits[0];
        log(name, &format!(
            "检索「{q}」首选（surface={}）：【{}】{} — {}",
            top.surface, top.space_name, top.label, top.excerpt.chars().take(40).collect::<String>()
        ));
    }

    // 6) incremental over UNCHANGED material is a zero-work no-op.
    let again = rt
        .space_build_llm(SUBJECT, &sp.id, true)
        .await
        .expect("incremental no-op must succeed");
    log(name, &format!(
        "增量空跑：targets={:?} published={} model_calls={}",
        again.targets, again.published.len(), again.model_calls
    ));
    assert!(again.targets.is_empty() && again.published.is_empty(),
        "unchanged material must not trigger rebuild: {:?}", again.targets);
    assert_eq!(again.model_calls, 0, "an incremental no-op never spends the model");

    log(name, "acc06 PASS —— 真实模型对纯知识空间「原始文档→镜像→深度构建→引用核验→检索→增量空跑」全链路通过；构建进度按钮的 GUI 走查属人工验收，不在本探针范围。");
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}
