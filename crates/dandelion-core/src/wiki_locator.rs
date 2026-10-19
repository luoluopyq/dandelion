//! Evidence locators as a first-class value (unification phase 4).
//!
//! A citation answers "where in the evidence is this claim". Until now the
//! only shape that existed was the code-centric `rel:Lx-Ly` token. A
//! knowledge space also cites a PDF page, a web page section, a session
//! turn, a whole directory tree — so the locator becomes an enum with one
//! honest `Unknown` floor, and the legacy token keeps a named variant with
//! *verbatim* parsing (the c20 regression red line lives through this
//! function, not beside it).
//!
//! Display syntax (also what [`crate::wiki_adapters`] emits):
//! - `src/a.ts:L10-L20` / `src/a.ts:L10` — legacy, first-class variant
//! - `guide.md#L1-L160` — line range on an adapter source
//! - `spec.pdf#p12` — page
//! - `notes.txt#C10-C40` — character range
//! - `https://example.com/docs#install` — URL with optional fragment
//! - `session:s123#t42`, `msg:s123#42` — conversation turn
//! - `note.md@sec:安装` — named section anchor
//! - `dir#tree`, `spec.pdf#pdf` — whole-body anchors (adapter output)
//!
//! Anything not matching is [`EvidenceLocator::Unknown`]: a locator we
//! cannot parse is carried and displayed as-is, never a panic and never a
//! silent drop. Round-trip holds: parsing a variant's display text yields
//! the same variant.

/// Where a claim points inside the evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceLocator {
    /// Legacy `rel:Lx-Ly` / `rel:Lx` token — the shape every published
    /// Wiki page already contains. Parsing is byte-for-byte the old
    /// `parse_citation_token`; do not loosen it.
    Legacy { rel: String, start: u32, end: u32 },
    /// `name.md#L10-L42` — line range on a non-legacy source.
    Line { source: String, start: u32, end: u32 },
    /// `name.txt#C10-C40` — character range.
    Char { source: String, start: u64, end: u64 },
    /// `spec.pdf#p12` — one page.
    Page { source: String, page: u32 },
    /// `https://…#frag` — web page, fragment optional.
    UrlFragment { url: String, fragment: Option<String> },
    /// `session:s123#t42` / `msg:s123#42` — one conversation turn.
    SessionTurn { session: String, turn: u32 },
    /// `note.md@sec:安装`, and whole-body anchors `dir#tree` / `spec.pdf#pdf`.
    Anchor { source: String, anchor: String },
    /// Not a locator we know. Carried verbatim so nothing is silently lost.
    Unknown { raw: String },
}

impl EvidenceLocator {
    /// Parse display text. **Legacy-first**: the old `rel:Lx-Ly` rules run
    /// verbatim before any new syntax, so every token the old parser
    /// accepted (and every token it rejected for a reason a new rule does
    /// not rescue) behaves exactly as before.
    pub fn from_display(s: &str) -> EvidenceLocator {
        if let Some((rel, start, end)) = parse_legacy(s) {
            return EvidenceLocator::Legacy { rel, start, end };
        }
        // URLs first: they own ':' and '#' in ways no other rule may grab.
        if s.starts_with("http://") || s.starts_with("https://") {
            let (url, frag) = match s.split_once('#') {
                Some((u, f)) if !f.is_empty() => (u.to_owned(), Some(f.to_owned())),
                _ => (s.to_owned(), None),
            };
            // A URL without any path beyond the scheme is not citable.
            if url.len() > "https://".len() {
                return EvidenceLocator::UrlFragment { url, fragment: frag };
            }
            return EvidenceLocator::Unknown { raw: s.to_owned() };
        }
        // Conversation turns.
        if let Some(rest) = s.strip_prefix("session:").or(s.strip_prefix("msg:")) {
            let is_msg = s.starts_with("msg:");
            let (session, turn) = match rest.split_once('#') {
                Some((sid, t)) => {
                    let digits = t.strip_prefix('t').unwrap_or(t);
                    (sid, digits)
                }
                None => (rest, ""),
            };
            if !session.is_empty() && !session.contains('#') {
                if let Ok(turn) = turn.parse::<u32>() {
                    return EvidenceLocator::SessionTurn { session: session.to_owned(), turn };
                }
                let _ = is_msg;
            }
            return EvidenceLocator::Unknown { raw: s.to_owned() };
        }
        // Named section anchor: `file@sec:name` (`@` never appears in the
        // legacy token, so this cannot shadow it).
        if let Some((source, anchor)) = s.split_once('@') {
            if !source.is_empty() && !anchor.is_empty() && !anchor.contains('@') {
                return EvidenceLocator::Anchor { source: source.to_owned(), anchor: anchor.to_owned() };
            }
            return EvidenceLocator::Unknown { raw: s.to_owned() };
        }
        // Hash-anchored forms: line / page / char / whole-body anchor.
        if let Some((source, frag)) = s.split_once('#') {
            if source.is_empty() || frag.is_empty() || frag.contains('#') {
                return EvidenceLocator::Unknown { raw: s.to_owned() };
            }
            if let Some(range) = frag.strip_prefix('L') {
                if let Some((a, b)) = parse_u32_range(range, 'L') {
                    return EvidenceLocator::Line { source: source.to_owned(), start: a, end: b };
                }
            } else if let Some(range) = frag.strip_prefix('C') {
                if let Some((a, b)) = parse_u64_range(range, 'C') {
                    return EvidenceLocator::Char { source: source.to_owned(), start: a, end: b };
                }
            } else if let Some(p) = frag.strip_prefix('p') {
                if let Ok(page) = p.parse::<u32>() {
                    return EvidenceLocator::Page { source: source.to_owned(), page };
                }
            }
            // `tree`, `pdf`, or any future named whole — an anchor.
            return EvidenceLocator::Anchor { source: source.to_owned(), anchor: frag.to_owned() };
        }
        EvidenceLocator::Unknown { raw: s.to_owned() }
    }

    /// Clickable display text. Round-trips through [`from_display`].
    pub fn display(&self) -> String {
        match self {
            EvidenceLocator::Legacy { rel, start, end } => {
                if start == end {
                    format!("{rel}:L{start}")
                } else {
                    format!("{rel}:L{start}-L{end}")
                }
            }
            EvidenceLocator::Line { source, start, end } => {
                if start == end {
                    format!("{source}#L{start}")
                } else {
                    format!("{source}#L{start}-L{end}")
                }
            }
            EvidenceLocator::Char { source, start, end } => {
                if start == end {
                    format!("{source}#C{start}")
                } else {
                    format!("{source}#C{start}-C{end}")
                }
            }
            EvidenceLocator::Page { source, page } => format!("{source}#p{page}"),
            EvidenceLocator::UrlFragment { url, fragment: Some(f) } => format!("{url}#{f}"),
            EvidenceLocator::UrlFragment { url, fragment: None } => url.clone(),
            EvidenceLocator::SessionTurn { session, turn } => format!("session:{session}#t{turn}"),
            EvidenceLocator::Anchor { source, anchor } => {
                // Whole-body anchors display with `#` (adapter form);
                // named sections with `@` (their input form).
                if anchor.contains(':') {
                    format!("{source}@{anchor}")
                } else {
                    format!("{source}#{anchor}")
                }
            }
            EvidenceLocator::Unknown { raw } => raw.clone(),
        }
    }

    /// The line-range read route: Legacy and Line both address numbered
    /// lines of a workspace-relative file, so the existing evidence fetch
    /// (index lookup + line window) serves either. Every other variant is
    /// a source type the line fetcher cannot answer; phase 5's source
    /// table routes those — claiming a route now would be a fabrication.
    pub fn as_line_range(&self) -> Option<(&str, u32, u32)> {
        match self {
            EvidenceLocator::Legacy { rel, start, end } => Some((rel, *start, *end)),
            EvidenceLocator::Line { source, start, end } => Some((source, *start, *end)),
            _ => None,
        }
    }

    /// True when this is not `Unknown` — callers that only render known
    /// citations use this to stay honest about the rest.
    pub fn is_known(&self) -> bool {
        !matches!(self, EvidenceLocator::Unknown { .. })
    }
}

impl std::fmt::Display for EvidenceLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

/// Parse `12` or `12-L20`-style range bodies (`"10"`, `"10-L20"` for L;
/// `"10"`, `"10-C20"` for C). The separator keeps its letter so display
/// round-trips exactly.
fn parse_u32_range(s: &str, letter: char) -> Option<(u32, u32)> {
    let (a, b) = match s.split_once(&format!("-{letter}")) {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let start = a.parse::<u32>().ok()?;
    let end = match b {
        Some(b) => b.parse::<u32>().ok()?,
        None => start,
    };
    Some((start, end))
}

fn parse_u64_range(s: &str, letter: char) -> Option<(u64, u64)> {
    let (a, b) = match s.split_once(&format!("-{letter}")) {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let start = a.parse::<u64>().ok()?;
    let end = match b {
        Some(b) => b.parse::<u64>().ok()?,
        None => start,
    };
    Some((start, end))
}

/// The legacy token parser, moved verbatim from `wiki_build` (phase 4):
/// `rel` must be repo-relative — no scheme, no drive, no leading slash,
/// no traversal — and the tail must be `Lx` or `Lx-Ly`. This exact shape
/// is the c20 regression contract; `from_display` runs it before anything
/// else so no new syntax ever steals a legacy token.
pub fn parse_legacy(t: &str) -> Option<(String, u32, u32)> {
    let idx = t.rfind(":L")?;
    let rel = &t[..idx];
    if rel.is_empty() || rel.contains(':') || rel.starts_with('/') || rel.contains("..") {
        return None;
    }
    let rest = &t[idx + 2..];
    let (s_str, e_str) = match rest.split_once("-L") {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let start = s_str.parse::<u32>().ok()?;
    let end = match e_str {
        Some(b) => b.parse::<u32>().ok()?,
        None => start,
    };
    Some((rel.to_owned(), start, end))
}
