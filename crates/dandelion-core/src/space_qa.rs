//! Phase 8: the unified knowledge-space read surface.
//!
//! One entry (`Runtime::space_search` / `Runtime::space_ask`) answers over an
//! EXPLICIT list of space ids and merges the three evidence surfaces that
//! previously only existed apart from each other:
//!
//! * `article` — the space's own Wiki articles (and its registered
//!   workspace's raw files, when one still exists on disk);
//! * `chunk`   — lexical hits over published, non-revoked document chunks;
//! * `vector`  — local sqlite-vec KNN over embedded chunks (deterministic
//!   local embedding: this leaves no device).
//!
//! Every listed space must pass `knowledge_authorize(Reader)` +
//! `knowledge_assert_scope` + `knowledge_external_decision(local_retrieval)`
//! BEFORE any hit is produced — a space the subject cannot read fails the
//! WHOLE call (fail-closed: a silent drop would leak which spaces exist and
//! which the caller is missing). `space_ask` is extractive by default, so a
//! `local_only` space still answers with citations; a model turn happens
//! only when the CALLER asks for one AND every listed space's policy allows
//! `cloud_summary`.

use serde::Serialize;

/// One citable piece of evidence, tagged with the space it came from and
/// which surface produced it. `label` is the human-facing citation line
/// (article/raw hits keep the shared [`crate::wiki::hit_label`] wording so
/// the same label travels through bridge, GUI and answers).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceEvidence {
    pub space_id: String,
    pub space_name: String,
    /// "article" | "chunk" | "vector"
    pub surface: String,
    pub label: String,
    pub excerpt: String,
    /// Immutable chunk locator for opening the original version in chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceSearchResult {
    pub hits: Vec<SpaceEvidence>,
    /// Spaces that were searched (access was granted) but produced no hit.
    /// Enumerated so the GUI can say WHERE it looked instead of a bare "no".
    pub empty_spaces: Vec<EmptySpace>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptySpace {
    pub space_id: String,
    pub space_name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceAnswer {
    pub answer: String,
    pub no_answer: bool,
    pub sources: Vec<SpaceEvidence>,
    /// Which mode actually produced `answer`: "local_extract" (excerpts +
    /// citations, always safe) or "cloud_summary" (model turn, policy-gated).
    pub mode: String,
    /// Honest note whenever policy or a failed model turn changed the path
    /// (e.g. cloud requested but `local_only` blocked it).
    pub policy_note: Option<String>,
}

/// First `max` BYTES of `s`, never splitting a UTF-8 character — chunk
/// content is arbitrary user text, and slicing on char floors after a byte
/// cap is the cheap, correct truncation every surface here needs.
pub(crate) fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Keep the matching passage, rather than always truncating the document's
/// introduction. Boundaries and caps are UTF-8 safe, including mixed scripts.
pub(crate) fn query_excerpt(text: &str, query: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes { return text.to_string(); }
    if max_bytes < 8 { return clip_bytes(text, max_bytes); }
    let lower = text.to_lowercase();
    let mut terms = crate::wiki::search_terms(query);
    terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
    let position = std::iter::once(query.to_lowercase()).chain(terms)
        .filter(|term| !term.is_empty())
        .find_map(|term| lower.find(&term));
    let Some(position) = position else { return clip_bytes(text, max_bytes); };
    // Translate the lowercase offset via character count before slicing the
    // original; Unicode lowercasing can change byte lengths.
    let mut lower_offset = 0usize;
    let match_at = text.char_indices().find_map(|(index, ch)| {
        if lower_offset >= position { return Some(index); }
        lower_offset += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        None
    }).unwrap_or(text.len());
    let mut start = match_at.saturating_sub(max_bytes / 4);
    while start > 0 && !text.is_char_boundary(start) { start -= 1; }
    let prefix = if start > 0 { "…" } else { "" };
    let available = max_bytes - prefix.len() - "…".len();
    let body = clip_bytes(&text[start..], available);
    let suffix = if start + body.len() < text.len() { "…" } else { "" };
    format!("{prefix}{body}{suffix}")
}

#[cfg(test)]
mod rag_excerpt_tests {
    use super::*;
    #[test]
    fn retrieves_the_answer_beyond_a_long_introduction_without_cutting_utf8() {
        let text = format!("{}\n保压时间不足会导致产品收缩。{}", "背景介绍。".repeat(300), "后文".repeat(300));
        let excerpt = query_excerpt(&text, "保压时间不足", 600);
        assert!(excerpt.contains("保压时间不足会导致产品收缩"));
        assert!(excerpt.len() <= 600);
        assert!(excerpt.starts_with('…'));
    }
}
