//! G15: generic knowledge-space planning — plan-v3 (unification phase 6).
//!
//! Pinned contract: a space with no project, no git, no source code and no
//! README still gets a real plan — the generic planning prompt is asserted
//! free of every software-only requirement (verbatim needle list below),
//! and its coverage floor (主题地图/内容结构/脉络流程/阅读指引) is enforced
//! in code, not by prompt compliance. The code profile is byte-frozen:
//! `plan_prompt_for` under "code" (and under "" = pre-v3) reproduces
//! `plan_prompt` exactly, pinned here by a sha256 golden. A recorded
//! profile switch re-plans **exactly once** — pre-v3 bases (empty profile)
//! are covered by the rules bump alone, and after the one re-plan the
//! new profile records clean. The generic article gate keeps the same
//! honesty contract over the wider locator grammar: line-form citations
//! keep the exact index+shown checks; page/char/anchor citations must name
//! an indexed source; URLs and session turns count as locatable-but-
//! unverifiable; an article with nothing traceable does not publish.
//!
//! Deterministic by design: zero model calls — the model never runs here
//! (real-model acceptance is recorded separately per the workorder).

use dandelion_core::wiki::{self, WikiProfile};
use dandelion_core::wiki_build::{
    self as wb, parse_plan_space, plan_basis_now, plan_prompt, PlanBasis, PlanBrief, ShownRange,
    StagedArticle,
};
use dandelion_core::wiki_index::{build_index, EvidenceIndex};
use std::path::Path;

fn w(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// A plain business/documentation workspace: documents and notes only.
/// No `src`, no manifest, no README, no git — the workorder's case zero.
fn generic_fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let ws = root.join("space");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    w(&ws, "notes/alpha.md", "一\n二\n三\n四\n五\n六\n");
    w(&ws, "notes/beta.txt", "beta 一\nbeta 二\nbeta 三\nbeta 四\n");
    w(&ws, "docs/report.txt", "报告 一\n报告 二\n报告 三\n报告 四\n报告 五\n");
    (ws, data)
}

/// A code workspace, for the byte-freeze and dispatch checks.
fn code_fixture(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let ws = root.join("repo");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    w(&ws, "src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n");
    w(&ws, "src/lib.rs", "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n");
    w(&ws, "notes/plan.txt", "计划 一\n计划 二\n");
    (ws, data)
}

fn index_of(ws: &Path, data: &Path) -> EvidenceIndex {
    build_index(data, ws, 100, 65_536).expect("index builds")
}

#[test]
fn docs_only_space_is_generic_and_plans_without_software_requirements() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = generic_fixture(dir.path());
    let prof = wiki::heuristic_profile(&ws);
    assert_eq!(prof.domain, WikiProfile::DOMAIN_GENERIC, "{prof:?}");
    assert!(!prof.is_code());

    let index = index_of(&ws, &data);
    assert!(!index.files.is_empty(), "fixture must index");
    assert!(index.files.iter().all(|f| f.rel != "README.md"));

    let brief = PlanBrief { profile: "generic".into(), facts: vec![] };
    let prompt = wb::plan_prompt_for(&brief, &index, 8);

    // Positive shape of the generic planner.
    assert!(prompt.contains("你是知识空间 Wiki 的规划者"));
    assert!(prompt.contains("这批资料整体讲什么"));
    assert!(prompt.contains("主题地图、内容结构、脉络流程、阅读指引"));
    assert!(prompt.contains("条目清单："));
    assert!(prompt.contains("个条目，本次纳入"));
    assert!(prompt.contains("想弄清一个重要问题该从哪些资料读起、关键结论出自哪里"));
    // The JSON output contract is identical, so every downstream seam stays
    // profile-blind.
    assert!(prompt.contains(r#"{"title":"主题名","focus":"这篇要回答什么""#));

    // Zero software requirements — every string the code planner asks for.
    for forbidden in [
        "你是项目知识库的规划者",
        "这个项目解决什么问题",
        "如何最小运行",
        "一次典型操作经过哪些模块",
        "关键代码在哪",
        "怎样验证改动",
        "不懂本仓库",
        "根目录有清单文件",
        "源码/入口/测试",
        "README",
        "仓库",
        "git",
        " 符号: ",
        "子目录中的独立项目",
        "其余文件从略",
        "不得整篇只依据 README 与文档写作",
    ] {
        assert!(!prompt.contains(forbidden), "generic plan prompt leaks {forbidden:?}:\n{prompt}");
    }
}

#[test]
fn facts_reach_the_generic_planner_only() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = generic_fixture(dir.path());
    let index = index_of(&ws, &data);
    let brief = PlanBrief {
        profile: "generic".into(),
        facts: vec!["客户：华东大区".into(), "来源：2026 年访谈记录".into()],
    };
    let prompt = wb::plan_prompt_for(&brief, &index, 8);
    assert!(prompt.contains("空间信息：\n- 客户：华东大区\n- 来源：2026 年访谈记录"));
    // The same brief under "code" is the byte-frozen planner: facts ignored.
    let code_brief = PlanBrief { profile: "code".into(), facts: brief.facts.clone() };
    let code_prompt = wb::plan_prompt_for(&code_brief, &index, 8);
    assert!(!code_prompt.contains("空间信息："));
    assert_eq!(code_prompt, plan_prompt(&index, 8));
}

#[test]
fn code_planner_is_byte_frozen_under_the_new_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = code_fixture(dir.path());
    assert!(wiki::heuristic_profile(&ws).is_code());
    let index = index_of(&ws, &data);

    let code = wb::plan_prompt_for(&PlanBrief { profile: "code".into(), facts: vec![] }, &index, 8);
    let empty = wb::plan_prompt_for(&PlanBrief::default(), &index, 8);
    let direct = plan_prompt(&index, 8);
    assert_eq!(code, direct, "code dispatch must reproduce plan_prompt verbatim");
    assert_eq!(empty, direct, "empty profile (pre-v3 callers) dispatches as code");
    assert!(direct.contains("你是项目知识库的规划者"));
    assert!(direct.contains("根目录有清单文件"));

    // sha256 golden: the planning prompt is deterministic (no timestamps),
    // so any drift in the code prompt is a regression, caught here.
    use sha2::{Digest, Sha256};
    let hex = format!("{:x}", Sha256::digest(direct.as_bytes()));
    assert_eq!(hex, PLAN_PROMPT_GOLDEN, "code plan_prompt bytes changed (golden mismatch)");

    // A generic brief over the same index plans differently by contract.
    let generic =
        wb::plan_prompt_for(&PlanBrief { profile: "generic".into(), facts: vec![] }, &index, 8);
    assert_ne!(generic, direct);
    assert!(generic.contains("你是知识空间 Wiki 的规划者"));
}

/// sha256 of `plan_prompt` over `code_fixture` above (max_articles 8).
/// Regenerate ONLY together with a deliberate, reviewed plan-v3 change.
const PLAN_PROMPT_GOLDEN: &str = "88e02e63b44857be97a4509c1f42eb2fd9e07042a5a5cfaccc254abf29938d6f";

#[test]
fn article_prompts_generic_desoftwareized_and_locator_wide() {
    let pack = "### 文件 notes/alpha.md\n（展示范围 L1-L6）";
    let a = wb::article_prompt_generic("脉络流程", "按什么顺序发生", pack);
    assert!(a.contains("你在为知识空间 Wiki 撰写《脉络流程》。这篇要回答：按什么顺序发生"));
    assert!(a.contains("这批资料里它和哪些内容相关"));
    assert!(a.contains("session:会话ID#t轮次"));
    assert!(a.contains("路径#p页码"));
    // The code-only import-inference clause is absent by design.
    assert!(!a.contains("描述一个被 import 的模块时"));
    let code_a = wb::article_prompt("核心流程", "怎么跑", pack);
    assert!(code_a.contains("描述一个被 import 的模块时"));

    let o = wb::overview_prompt_generic(&[("F.md".into(), "主题地图".into())], pack);
    assert!(o.contains("你在为知识空间 Wiki 撰写《项目概览》。读者完全不了解这批资料。"));
    assert!(o.contains("## 学习顺序"));
    assert!(o.contains("- [主题地图](F.md)"));
    assert!(!o.contains("如何以最小方式跑起来"));
}

#[test]
fn generic_plan_floor_is_enforced_in_code() {
    // A model that ignores the coverage requirement still gets the floor.
    let raw = r#"[{"title":"随便写的主题","focus":"x","inputs":["notes/alpha.md"]}]"#;
    let topics = parse_plan_space(raw, 8, false).unwrap();
    let titles: Vec<&str> = topics.iter().map(|t| t.title.as_str()).collect();
    for req in ["主题地图", "内容结构", "脉络流程", "阅读指引"] {
        assert!(titles.contains(&req), "{titles:?}");
    }
    // The generic floor does not smuggle in the software floor.
    for code_req in wb::REQUIRED_TOPICS {
        assert!(!titles.contains(&code_req), "{titles:?}");
    }
    let map = topics.iter().find(|t| t.title == "脉络流程").unwrap();
    assert_eq!(map.focus, "事情的来龙去脉：从头到尾按什么顺序发生、为什么");

    // The code floor still applies under code (regression).
    let code_topics = parse_plan_space(raw, 10, true).unwrap();
    let code_titles: Vec<&str> = code_topics.iter().map(|t| t.title.as_str()).collect();
    for req in wb::REQUIRED_TOPICS {
        assert!(code_titles.contains(&req), "{code_titles:?}");
    }
    assert!(!code_titles.contains(&"主题地图"));

    // Fence tolerance + clamping are shared.
    let fenced = format!("```json\n{raw}\n```");
    assert!(!parse_plan_space(&fenced, 8, false).unwrap().is_empty());
    let many = r#"[
        {"title":"甲","focus":"","inputs":[]},{"title":"乙","focus":"","inputs":[]},
        {"title":"丙","focus":"","inputs":[]},{"title":"丁","focus":"","inputs":[]},
        {"title":"戊","focus":"","inputs":[]}
    ]"#;
    let clamped = parse_plan_space(many, 2, false).unwrap();
    assert_eq!(clamped.len(), 2);
    assert_eq!(clamped[0].title, "甲");
    assert_eq!(clamped[1].title, "乙");
}

#[test]
fn profile_switch_replans_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = code_fixture(dir.path());
    let index = index_of(&ws, &data);

    let basis = plan_basis_now(&index, "(default)", "fp", "code", vec![]);
    assert_eq!(basis.rules, wb::PLAN_RULES_VERSION);
    assert_eq!(basis.rules, "plan-v3");
    assert_eq!(basis.profile, "code");

    // Fresh v3 basis, same profile: nothing forces a replan.
    let ch = wb::changed_files_from_basis_for_profile(&basis, &index, "(default)", "fp", Some("code"));
    assert!(!ch.rules_changed && !ch.profile_changed && !ch.model_changed && !ch.settings_changed);

    // Pre-v3 basis (rules v2, empty profile): the rules bump re-plans the
    // one time; profile_changed stays false so nothing double-fires.
    let prev3 = PlanBasis { rules: "plan-v2".into(), profile: String::new(), ..basis.clone() };
    let ch = wb::changed_files_from_basis_for_profile(&prev3, &index, "(default)", "fp", Some("code"));
    assert!(ch.rules_changed && !ch.profile_changed);
    let ch = wb::changed_files_from_basis_for_profile(&prev3, &index, "(default)", "fp", Some("generic"));
    assert!(ch.rules_changed && !ch.profile_changed, "pre-v3 covered by rules bump alone");

    // A recorded switch code→generic fires once, rules stay clean.
    let ch =
        wb::changed_files_from_basis_for_profile(&basis, &index, "(default)", "fp", Some("generic"));
    assert!(ch.profile_changed && !ch.rules_changed);

    // After the replan is recorded under the new profile: quiet again.
    let g_basis = plan_basis_now(&index, "(default)", "fp", "generic", vec![]);
    let ch =
        wb::changed_files_from_basis_for_profile(&g_basis, &index, "(default)", "fp", Some("generic"));
    assert!(!ch.profile_changed && !ch.rules_changed, "no thrash after the one replan");

    // The profile-blind wrapper (all pre-phase-6 callers) never reports it.
    let ch = wb::changed_files_from_basis(&basis, &index, "(default)", "fp");
    assert!(!ch.profile_changed);

    // Old plan-basis.json files (written before phase 6) deserialize with
    // profile "" and camelCase keys intact.
    let mut obj: serde_json::Value = serde_json::to_value(&basis).unwrap();
    assert_eq!(obj["profile"], "code");
    assert!(obj.get("savedAt").is_some(), "camelCase contract");
    obj.as_object_mut().unwrap().remove("profile");
    let legacy: PlanBasis = serde_json::from_value(obj).unwrap();
    assert_eq!(legacy.profile, "");
    let ch = wb::changed_files_from_basis_for_profile(&legacy, &index, "(default)", "fp", Some("code"));
    assert!(!ch.profile_changed);
}

#[test]
fn generic_article_gate_keeps_the_honesty_contract() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = generic_fixture(dir.path());
    let index = index_of(&ws, &data);
    let shown = [ShownRange { rel: "notes/alpha.md".into(), start: 1, end: 4 }];

    // Line-form keeps the exact code checks: index + shown window.
    let ok = wb::verify_article_generic(
        &index,
        "这批资料讲流程。见 `notes/alpha.md:L1-L3`；清单概览 `docs/report.txt#p2`；外部背景 `https://example.com/x#frag`。",
        &shown,
    )
    .expect("line+page+URL article passes");
    assert_eq!(ok, vec![("notes/alpha.md".to_string(), 1, 3)]);

    // #L display syntax is the same check via the Line variant.
    wb::verify_article_generic(&index, "见 `notes/alpha.md#L1-L3`。", &shown).unwrap();

    // Line outside the shown window is still a fabricated proof.
    let e = wb::verify_article_generic(&index, "见 `notes/alpha.md:L5-L6`。", &shown)
        .unwrap_err();
    assert!(e.contains("引用了未展示给模型的范围"), "{e}");
    // Out-of-range and unknown-file line cites keep the code strings.
    let e = wb::verify_article_generic(&index, "见 `notes/alpha.md:L1-L99`。", &shown).unwrap_err();
    assert!(e.contains("引用范围越界"), "{e}");
    let e = wb::verify_article_generic(&index, "见 `ghost.md:L1-L2`。", &shown).unwrap_err();
    assert!(e.contains("引用了证据索引之外的文件：ghost.md"), "{e}");

    // Page/char/anchor must name a source the index has.
    let e = wb::verify_article_generic(&index, "见 `spec.pdf#p3`。", &shown).unwrap_err();
    assert!(e.contains("引用了证据索引之外的来源：spec.pdf"), "{e}");
    wb::verify_article_generic(&index, "见 `docs/report.txt#p2` 与 `notes/beta.txt@sec:总览`。", &shown)
        .unwrap();

    // URLs and session turns are locatable but unverifiable: known, not bad.
    let cites =
        wb::verify_article_generic(&index, "外部依据 `https://example.com/a` 与 `session:s123#t42`。", &shown)
            .expect("URL/session cites count");
    assert!(cites.is_empty());

    // The workorder rule: nothing traceable → does not publish.
    let e = wb::verify_article_generic(&index, "整体是一批说明材料，读起来很顺。", &shown).unwrap_err();
    assert_eq!(e, "没有任何可溯源定位（路径/URL/会话），无法溯源");
    assert_eq!(wb::verify_article_generic(&index, "   ", &shown).unwrap_err(), "文章为空");
    assert_eq!(
        wb::verify_article_generic(&index, "正文 `notes/alpha.md:L1-L1` ```", &shown).unwrap_err(),
        "代码块未闭合（疑似输出被截断）"
    );
}

#[test]
fn generic_audit_finds_every_untraceable_cite_for_demotion() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = generic_fixture(dir.path());
    let index = index_of(&ws, &data);
    let shown = [ShownRange { rel: "notes/alpha.md".into(), start: 1, end: 4 }];

    let bads = wb::find_invalid_citations_space(
        &index,
        &shown,
        "见 `ghost.pdf#p7`、`notes/alpha.md:L2-L3` 与 `https://example.com/z`。",
        false,
    );
    assert_eq!(bads.len(), 1, "{bads:?}");
    assert_eq!(bads[0].rel, "ghost.pdf");
    assert_eq!(bads[0].line_start, 7, "page number rides along for the marker");
    assert!(bads[0].raw.contains("ghost.pdf"));

    // Line cites outside the window are audited the same as the code path.
    let bads = wb::find_invalid_citations_space(&index, &shown, "见 `notes/alpha.md:L5-L6`。", false);
    assert_eq!(bads.len(), 1);

    // code=true keeps the legacy audit verbatim: URL tokens are prose there.
    let bads = wb::find_invalid_citations_space(&index, &shown, "见 `https://example.com/z`。", true);
    assert!(bads.is_empty());
}

#[test]
fn generic_build_gate_wraps_the_generic_article_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (ws, data) = generic_fixture(dir.path());
    let index = index_of(&ws, &data);

    let empty: Vec<StagedArticle> = vec![];
    assert_eq!(
        wb::verify_build_space(&index, &empty, false).unwrap_err(),
        "构建没有产出任何文章"
    );
    assert_eq!(wb::verify_build_space(&index, &empty, true).unwrap_err(), "构建没有产出任何文章");

    let good = StagedArticle {
        file: "TOPIC-map.md".into(),
        title: "主题地图".into(),
        focus: "整体讲什么".into(),
        content: "这批资料围绕流程展开，主线见 `notes/alpha.md:L1-L3`。".into(),
        inputs: vec![],
        ranges: vec![ShownRange { rel: "notes/alpha.md".into(), start: 1, end: 4 }],
    };
    wb::verify_build_space(&index, &[good.clone()], false).unwrap();

    // First error wins, all-or-nothing — same verdict shape as code.
    let bad = StagedArticle { title: "脉络流程".into(), content: "无定位散文。".into(), ..good };
    let e = wb::verify_build_space(&index, &[bad], false).unwrap_err();
    assert!(e.starts_with("《脉络流程》: 没有任何可溯源定位"), "{e}");
}
