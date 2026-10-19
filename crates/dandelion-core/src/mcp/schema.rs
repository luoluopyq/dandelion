use super::names::{collides_with_builtin, mcp_tool_name};
use crate::config::Settings;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct SchemaCandidate {
    pub server_id: String,
    pub remote_name: String,
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub read_only: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SchemaBudgetResult {
    pub kept: Vec<SchemaCandidate>,
    pub skipped: Vec<String>,
}

pub fn candidate_from_mcp(
    server_id: &str,
    remote_name: &str,
    description: String,
    parameters: Value,
    read_only: bool,
) -> Option<SchemaCandidate> {
    let name = mcp_tool_name(server_id, remote_name);
    if collides_with_builtin(&name) {
        tracing::warn!(
            server = server_id,
            tool = remote_name,
            "mcp tool dropped: collides with builtin"
        );
        return None;
    }
    Some(SchemaCandidate {
        server_id: server_id.to_string(),
        remote_name: remote_name.to_string(),
        name,
        description,
        parameters,
        read_only,
    })
}

pub fn apply_schema_budget(
    candidates: Vec<SchemaCandidate>,
    settings: &Settings,
) -> SchemaBudgetResult {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut total = 0usize;
    let soft = settings.mcp_max_schema_bytes;
    for mut c in candidates {
        c.description = truncate_desc(&c.description, settings.mcp_description_max_chars);
        let bytes = function_object_bytes(&c);
        if bytes > settings.mcp_max_tool_schema_bytes {
            tracing::warn!(
                tool = %c.name,
                bytes,
                cap = settings.mcp_max_tool_schema_bytes,
                "mcp tool skipped: over maxToolSchemaBytes"
            );
            skipped.push(c.name);
            continue;
        }
        if soft > 0 && total.saturating_add(bytes) > soft {
            tracing::warn!(
                tool = %c.name,
                total,
                cap = soft,
                "mcp tool skipped: over maxSchemaBytes"
            );
            skipped.push(c.name);
            continue;
        }
        total += bytes;
        kept.push(c);
    }
    SchemaBudgetResult { kept, skipped }
}

fn truncate_desc(desc: &str, max_chars: usize) -> String {
    let n = desc.chars().count();
    if n <= max_chars {
        return desc.to_string();
    }
    let mut s: String = desc.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

pub(crate) fn function_object_bytes(c: &SchemaCandidate) -> usize {
    let v = serde_json::json!({
        "name": c.name,
        "description": c.description,
        "parameters": c.parameters,
    });
    serde_json::to_vec(&v)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use serde_json::json;

    fn cand(name: &str, desc: &str, params: Value) -> SchemaCandidate {
        SchemaCandidate {
            server_id: "echo".into(),
            remote_name: name.into(),
            name: format!("mcp__echo__{name}"),
            description: desc.into(),
            parameters: params,
            read_only: false,
        }
    }

    #[test]
    fn per_tool_hard_cap_skips_one() {
        let mut s = Settings::defaults();
        s.mcp_max_tool_schema_bytes = 200;
        s.mcp_max_schema_bytes = 0;
        let huge = cand(
            "fat",
            "ok",
            json!({"type":"object","properties":{"x":{"type":"string","enum": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]}}}),
        );
        let small = cand("ping", "pong", json!({"type":"object"}));
        let r = apply_schema_budget(vec![huge, small.clone()], &s);
        assert!(r.skipped.iter().any(|n| n.contains("fat")));
        assert_eq!(r.kept.len(), 1);
        assert_eq!(r.kept[0].name, small.name);
    }

    #[test]
    fn description_truncated_schema_untouched() {
        let mut s = Settings::defaults();
        s.mcp_description_max_chars = 8;
        s.mcp_max_schema_bytes = 0;
        let params = json!({"type":"object","properties":{"keep":{"type":"string"}}});
        let r = apply_schema_budget(vec![cand("ping", "abcdefghijklmnop", params.clone())], &s);
        assert_eq!(r.kept.len(), 1);
        assert!(r.kept[0].description.ends_with('…'));
        assert!(r.kept[0].description.chars().count() <= 8);
        assert_eq!(r.kept[0].parameters, params);
    }

    #[test]
    fn total_soft_cap_keeps_already_admitted() {
        let mut s = Settings::defaults();
        s.mcp_max_tool_schema_bytes = 16_384;
        s.mcp_description_max_chars = 1024;
        s.mcp_max_schema_bytes = 80;
        let a = cand("a", "x", json!({"type":"object"}));
        let b = cand("b", "y", json!({"type":"object"}));
        let c = cand("c", "z", json!({"type":"object"}));
        let r = apply_schema_budget(vec![a, b, c], &s);
        assert!(!r.kept.is_empty());
        assert!(!r.skipped.is_empty() || r.kept.len() < 3);
        assert!(r.kept.len() + r.skipped.len() == 3);
    }

    #[test]
    fn zero_total_cap_keeps_all_under_hard_cap() {
        let mut s = Settings::defaults();
        s.mcp_max_schema_bytes = 0;
        s.mcp_max_tool_schema_bytes = 16_384;
        let r = apply_schema_budget(
            vec![
                cand("a", "x", json!({"type":"object"})),
                cand("b", "y", json!({"type":"object"})),
            ],
            &s,
        );
        assert_eq!(r.kept.len(), 2);
        assert!(r.skipped.is_empty());
    }

    #[test]
    fn candidate_drops_only_exact_builtin_name() {
        let c = candidate_from_mcp(
            "echo",
            "read_file",
            "d".into(),
            json!({"type":"object"}),
            true,
        )
        .expect("prefixed name is not a builtin");
        assert_eq!(c.name, "mcp__echo__read_file");
        assert!(c.read_only);
        assert!(candidate_from_mcp("", "", String::new(), json!({}), false).is_some());
    }

    #[test]
    fn unicode_description_counts_scalars() {
        let mut s = Settings::defaults();
        s.mcp_description_max_chars = 3;
        s.mcp_max_schema_bytes = 0;
        let r = apply_schema_budget(vec![cand("ping", "你好世界", json!({"type":"object"}))], &s);
        assert_eq!(r.kept[0].description.chars().count(), 3);
        assert!(r.kept[0].description.ends_with('…'));
    }

    #[test]
    fn description_at_cap_not_truncated() {
        let mut s = Settings::defaults();
        s.mcp_description_max_chars = 4;
        s.mcp_max_schema_bytes = 0;
        let r = apply_schema_budget(vec![cand("ping", "abcd", json!({"type":"object"}))], &s);
        assert_eq!(r.kept[0].description, "abcd");
    }

    #[test]
    fn budget_preserves_admission_order_and_read_only() {
        let mut s = Settings::defaults();
        s.mcp_max_schema_bytes = 0;
        let mut a = cand("b-second", "x", json!({"type":"object"}));
        a.read_only = true;
        let b = cand("a-first", "y", json!({"type":"object"}));
        let r = apply_schema_budget(vec![a, b], &s);
        assert_eq!(r.kept[0].name, "mcp__echo__b-second");
        assert!(r.kept[0].read_only);
        assert_eq!(r.kept[1].name, "mcp__echo__a-first");
        assert!(!r.kept[1].read_only);
    }
}
