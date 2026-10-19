use super::scan::SkillDoc;
use crate::config::Settings;

pub fn inject_skills(selected: &[SkillDoc], settings: &Settings) -> (String, Vec<String>) {
    let mut chosen: Vec<&SkillDoc> = selected.iter().filter(|d| !d.framework).collect();
    chosen.sort_by(|a, b| a.id.cmp(&b.id));
    let cap = settings.skills_max_inject_bytes;
    let mut out = String::new();
    let mut skipped = Vec::new();
    for doc in chosen {
        let piece = doc.raw.trim().to_string();
        let next = if out.is_empty() {
            format!("已启用技能全文：\n{piece}")
        } else {
            format!("{out}\n\n{piece}")
        };
        if next.len() > cap {
            skipped.push(doc.id.clone());
            tracing::warn!(skill = %doc.id, cap, "skill skipped: over maxInjectBytes");
            continue;
        }
        out = next;
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::scan::SkillDoc;
    use std::path::PathBuf;

    fn doc(id: &str, raw: &str) -> SkillDoc {
        SkillDoc {
            id: id.into(),
            description: id.into(),
            body: raw.into(),
            raw: raw.into(),
            path: PathBuf::from("SKILL.md"),
            framework: false,
        }
    }

    #[test]
    fn skips_whole_skill_over_cap() {
        let mut s = crate::config::Settings::defaults();
        s.skills_max_inject_bytes = 1024;
        let big = "x".repeat(2000);
        let (text, skipped) = inject_skills(
            &[doc("tiny", "---\nname: tiny\n---\nok"), doc("huge", &big)],
            &s,
        );
        assert!(text.contains("tiny") || text.is_empty() || skipped.contains(&"huge".to_string()));
        assert!(skipped.contains(&"huge".to_string()));
        assert!(text.contains("tiny"));
        assert!(!text.contains(&big));
    }

    fn fw(id: &str, raw: &str) -> SkillDoc {
        let mut d = doc(id, raw);
        d.framework = true;
        d
    }

    #[test]
    fn framework_never_injected() {
        let s = crate::config::Settings::defaults();
        let (text, skipped) = inject_skills(&[fw("wiki", "---\nname: wiki\n---\nSECRET_FW")], &s);
        assert!(text.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn sorts_by_id_and_packs_smaller_after_skip() {
        let mut s = crate::config::Settings::defaults();
        s.skills_max_inject_bytes = 80;
        let huge = "H".repeat(200);
        let (text, skipped) = inject_skills(
            &[
                doc("z-big", &huge),
                doc("a-tiny", "---\nname: a-tiny\n---\nok"),
            ],
            &s,
        );
        assert!(skipped.contains(&"z-big".to_string()));
        assert!(text.contains("a-tiny"), "{text}");
        assert!(!text.contains(&huge));
        assert!(text.starts_with("已启用技能全文："));
    }

    #[test]
    fn empty_selection_omits_segment() {
        let s = crate::config::Settings::defaults();
        let (text, skipped) = inject_skills(&[], &s);
        assert!(text.is_empty());
        assert!(skipped.is_empty());
    }
}
