//! Source adapters: turn a raw source (file, web page, directory, PDF)
//! into content chunks with locatable positions, under one honest contract
//! (unification phase 3).
//!
//! Three rules hold across every adapter:
//! 1. **Nothing is silently dropped.** Anything the adapter could not use
//!    lands in [`ParseReport::skipped`] with the reason — an unreadable
//!    encoding, a broken PDF, a truncated read. The report is the audit
//!    trail; an empty `chunks` with an empty `skipped` never happens.
//! 2. **A probe miss is data, not an error.** [`probe`] returning `None`
//!    and [`extract_source`] answering with an `unsupported` kind keep the
//!    capability list honest: the pipeline records "we cannot read .zip
//!    yet" instead of pretending or crashing.
//! 3. **Locators are display strings in the phase-4 syntax** —
//!    `name.md#L10-L42` for line ranges, `name.pdf#pdf` for a PDF body,
//!    `dir#tree` for a tree listing. Phase 4 promotes these to the
//!    first-class `EvidenceLocator` enum; the legacy `rel:Lx-Ly` token
//!    remains untouched and parseable either way.
//!
//! PDF extraction used to live only in the Tauri host; it is a shared
//! product capability now, so it lives here and the desktop calls into
//! core (one implementation, one dependency).

use crate::error::{Error, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Which extraction pipeline produced a stored version's content (§三:
/// an immutable version records its parser version alongside its hash).
/// Baseline `1`: raw passthrough text, PDF text, and the html body-text
/// snapshot. Bump it whenever the output of ANY adapter changes meaning,
/// so an old citation still resolves against the content the old parser
/// actually stored — never reinterpreted through new semantics.
pub const PARSER_VERSION: i64 = 1;

/// One addressable block of extracted content.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentChunk {
    /// Display locator in the phase-4 syntax, e.g. `guide.md#L1-L40`.
    pub locator: String,
    pub text: String,
}

/// Honest accounting of one extraction. `skipped` must explain every
/// reason content is missing; `truncated` says the source was cut at the
/// byte budget rather than read whole.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParseReport {
    /// Total bytes of source text actually carried into `chunks`.
    pub ok_bytes: u64,
    /// The source exceeded the byte budget; only the head was extracted.
    pub truncated: bool,
    /// Human-readable reasons for everything NOT in `chunks`, in Chinese
    /// (user-facing product surface, matching the desktop's voice).
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// The result of adapting one source.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceContent {
    /// Adapter kind: `text`, `html`, `dir_tree`, `pdf`, or `unsupported`.
    pub source_kind: String,
    pub chunks: Vec<ContentChunk>,
    pub report: ParseReport,
}

/// One source type the Wiki can ingest. Adding web pages, audio
/// transcripts or docx later means adding an adapter — the pipeline,
/// evidence store and citation layer do not change.
pub trait SourceAdapter: Send + Sync {
    /// Stable capability name, recorded as `source_kind` downstream.
    fn kind(&self) -> &'static str;
    /// Probe by path (extension / dir-ness) only — never opens the file.
    fn supports(&self, path: &Path) -> bool;
    /// Extract content chunks under a byte budget. `Err` is reserved for
    /// the infrastructure failing (should not normally happen); content
    /// that cannot be *parsed* is reported through `ParseReport::skipped`
    /// as an `Ok` with empty chunks — broken input must never take down a
    /// build of the other 200 sources.
    fn extract(&self, path: &Path, max_bytes: usize) -> Result<SourceContent>;
}

/// The capability list, in probe priority order. Text first so `.md`/`.txt`
/// never reach a heavier adapter; `DirTree` takes directories.
pub fn adapters() -> Vec<Arc<dyn SourceAdapter>> {
    vec![
        Arc::new(TextAdapter),
        Arc::new(HtmlAdapter),
        Arc::new(PdfAdapter),
        Arc::new(DirTreeAdapter),
    ]
}

/// Kinds this build can handle — shown to the user so an unsupported
/// source is a named gap, not a silent drop.
pub fn capability_kinds() -> Vec<&'static str> {
    adapters().iter().map(|a| a.kind()).collect()
}

/// First adapter that claims this path.
pub fn probe(path: &Path) -> Option<Arc<dyn SourceAdapter>> {
    adapters().into_iter().find(|a| a.supports(path))
}

/// Extract through whichever adapter claims the path; an unclaimed path
/// yields an `unsupported` content with the reason, so callers can batch
/// mixed sources without special-casing misses.
pub fn extract_source(path: &Path, max_bytes: usize) -> Result<SourceContent> {
    match probe(path) {
        Some(a) => a.extract(path, max_bytes),
        None => Ok(SourceContent {
            source_kind: "unsupported".into(),
            chunks: Vec::new(),
            report: ParseReport {
                skipped: vec![format!(
                    "暂不支持该文件类型：{}（当前能力：{}）",
                    path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("(无扩展名)"),
                    capability_kinds().join("、")
                )],
                ..Default::default()
            },
        }),
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn ext_is(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

// ------------------------------------------------------------------- text

/// Plain-text documents (markdown, rst, adoc, txt): read as UTF-8 and
/// chunked by line range so every chunk carries a clickable `#Lx-Ly`.
pub struct TextAdapter;

/// Lines per text chunk — sized so a chunk is a coherent reading unit
/// and a citation range stays short.
const TEXT_CHUNK_LINES: usize = 160;

impl SourceAdapter for TextAdapter {
    fn kind(&self) -> &'static str {
        "text"
    }

    fn supports(&self, path: &Path) -> bool {
        ext_is(
            path,
            &["md", "mdx", "markdown", "rst", "txt", "adoc"],
        )
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<SourceContent> {
        let name = display_name(path);
        let mut report = ParseReport::default();
        let Some(text) = read_body_utf8(path, max_bytes, &mut report) else {
            return Ok(SourceContent {
                source_kind: self.kind().into(),
                chunks: Vec::new(),
                report,
            });
        };
        let lines: Vec<&str> = text.lines().collect();
        let chunks = line_chunks(&name, &lines, &mut report);
        if text.trim().is_empty() && report.skipped.is_empty() {
            report.skipped.push(format!("{name} 内容为空"));
        }
        Ok(SourceContent {
            source_kind: self.kind().into(),
            chunks,
            report,
        })
    }
}

/// Read a file under a byte budget, decoding UTF-8 honestly: a head our
/// own truncation split mid-character is retried at the last complete
/// character (truncation reported), a file that is not UTF-8 at all is
/// reported — both leave `report` ready for an empty-chunk answer.
fn read_body_utf8(path: &Path, max_bytes: usize, report: &mut ParseReport) -> Option<String> {
    let name = display_name(path);
    let raw = match fs::read(path) {
        Ok(r) => r,
        Err(e) => {
            report.skipped.push(format!("无法读取 {name}：{e}"));
            return None;
        }
    };
    let capped = raw.len() > max_bytes;
    let used = &raw[..raw.len().min(max_bytes)];
    let text = match std::str::from_utf8(used) {
        Ok(t) => t.trim_start_matches('\u{feff}').to_owned(),
        Err(_) => {
            // A truncated head can split a multi-byte character; that
            // is our truncation's fault, so retry at the exact valid
            // prefix before blaming the file. Nothing valid at all is
            // the file's fault, and the reason is reported as such.
            let head = floor_char_boundary(used);
            if head == 0 && !used.is_empty() {
                report.skipped.push(format!("{name} 不是有效的 UTF-8 文本"));
                return None;
            }
            report.truncated = capped;
            std::str::from_utf8(&used[..head])
                .unwrap_or("")
                .trim_start_matches('\u{feff}')
                .to_owned()
        }
    };
    if capped {
        report.truncated = true;
        report
            .skipped
            .push(format!("{name} 超出字节预算，仅提取前 {max_bytes} 字节"));
    }
    Some(text)
}

/// Chunk extracted lines into reading-sized blocks, each carrying a
/// clickable `name#Lx-Ly` display locator; `ok_bytes` counts exactly what
/// the chunks carry.
fn line_chunks(name: &str, lines: &[&str], report: &mut ParseReport) -> Vec<ContentChunk> {
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + TEXT_CHUNK_LINES).min(lines.len());
        let body = lines[start..end].join("\n");
        report.ok_bytes += body.len() as u64;
        chunks.push(ContentChunk {
            locator: format!("{name}#L{}-L{}", start + 1, end),
            text: body,
        });
        start = end;
    }
    chunks
}

/// Largest prefix of `bytes` that is valid UTF-8. `valid_up_to` is exact
/// for both failure shapes — a continuation byte in a lead position AND a
/// lead byte whose sequence is cut off by the slice end (our truncation).
fn floor_char_boundary(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) => e.valid_up_to(),
    }
}

// ------------------------------------------------------------------- html

/// Web pages ingested as a *body-text snapshot*: the visible text of the
/// document, one line per block element (paragraph, heading, list item,
/// table cell), with markup, scripts and styles stripped and entities
/// decoded. The stored version content IS the snapshot, so a citation
/// `page.html:L4-L11` resolves to snapshot lines exactly like a text
/// citation resolves to file lines — and because headings land on their
/// own lines, 章节 are addressable by their snapshot line range.
///
/// The extraction is deliberately naive (no HTML parser dependency): it
/// handles the shapes real saved pages actually have — quoted `>` inside
/// attributes, `</script>`-lookalikes inside script bodies, unclosed
/// `<head>` — and never fails: the worst case is an empty snapshot plus
/// the report's reason, the same contract every adapter keeps.
pub struct HtmlAdapter;

impl SourceAdapter for HtmlAdapter {
    fn kind(&self) -> &'static str {
        "html"
    }

    fn supports(&self, path: &Path) -> bool {
        ext_is(path, &["html", "htm", "xhtml"])
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<SourceContent> {
        let name = display_name(path);
        let mut report = ParseReport::default();
        let Some(text) = read_body_utf8(path, max_bytes, &mut report) else {
            return Ok(SourceContent {
                source_kind: self.kind().into(),
                chunks: Vec::new(),
                report,
            });
        };
        let snapshot = html_snapshot(&text);
        if snapshot.is_empty() {
            if report.skipped.is_empty() {
                report
                    .skipped
                    .push(format!("{name} 未提取到网页正文（可能只有脚本与样式）"));
            }
            return Ok(SourceContent {
                source_kind: self.kind().into(),
                chunks: Vec::new(),
                report,
            });
        }
        let lines: Vec<&str> = snapshot.lines().collect();
        let chunks = line_chunks(&name, &lines, &mut report);
        Ok(SourceContent {
            source_kind: self.kind().into(),
            chunks,
            report,
        })
    }
}

/// Canonicalise an HTML document into snapshot lines: visible text only,
/// one line per block element, whitespace runs collapsed to a single
/// space as a browser would, character references decoded. The bodies of
/// raw-text elements (`script`, `style`, `template`, `noscript`,
/// `textarea`, `svg`) are never page text and are skipped whole.
pub fn html_snapshot(text: &str) -> String {
    let b = text.as_bytes();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    // Lowercased name of the raw-text element whose body we are skipping.
    let mut raw: Option<String> = None;
    while i < b.len() {
        if let Some(tag) = &raw {
            match find_close_tag(text, i, tag) {
                Some(lt) => {
                    i = lt;
                    raw = None;
                }
                // An unclosed raw element swallows only to end of input —
                // everything already collected survives.
                None => break,
            }
            continue;
        }
        match b[i] {
            b'<' if starts_with_ci(b, i, b"<!--") => {
                i = match find_comment_end(b, i + 4) {
                    Some(end) => end,
                    None => break,
                };
            }
            // DOCTYPE, CDATA-openers and processing instructions carry no
            // page text; skip to the next `>`.
            b'<' if matches!(b.get(i + 1), Some(b'!' | b'?')) => {
                i = find_byte(b, i + 2, b'>').map(|p| p + 1).unwrap_or(b.len());
            }
            b'<' if matches!(b.get(i + 1), Some(c) if c.is_ascii_alphabetic() || *c == b'/') => {
                let closing = b[i + 1] == b'/';
                let name_start = i + if closing { 2 } else { 1 };
                let mut k = name_start;
                while k < b.len() && (b[k] as char).is_ascii_alphanumeric() {
                    k += 1;
                }
                let name = text[name_start..k].to_ascii_lowercase();
                let end = skip_tag(b, k);
                // `<tag ... />` is self-closed: it opens nothing whose
                // body could follow.
                let self_closed = end >= 2 && b[end - 2] == b'/';
                if is_block_tag(&name) {
                    flush_snapshot_line(&mut lines, &mut cur);
                }
                if !closing && !self_closed && is_raw_tag(&name) {
                    raw = Some(name);
                }
                i = end;
            }
            b'&' => {
                let (decoded, used) = decode_entity(&text[i..]);
                for ch in decoded.chars() {
                    push_snapshot_char(&mut cur, ch);
                }
                i += used;
            }
            _ => {
                let ch = text[i..].chars().next().expect("boundary checked");
                push_snapshot_char(&mut cur, ch);
                i += ch.len_utf8();
            }
        }
    }
    flush_snapshot_line(&mut lines, &mut cur);
    lines.join("\n")
}

/// One text character into the current snapshot line: whitespace runs
/// collapse to a single space and a line never starts with one.
fn push_snapshot_char(cur: &mut String, ch: char) {
    if ch.is_whitespace() || ch == '\u{a0}' || ch == '\u{feff}' {
        if !cur.is_empty() && !cur.ends_with(' ') {
            cur.push(' ');
        }
    } else {
        cur.push(ch);
    }
}

fn flush_snapshot_line(lines: &mut Vec<String>, cur: &mut String) {
    let t = cur.trim_end();
    if !t.is_empty() {
        lines.push(t.to_owned());
    }
    cur.clear();
}

/// Elements whose opening or close BREAKS the current snapshot line —
/// the visible block structure of a page.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "caption"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "option"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

/// Elements whose entire BODY is never page text (code, styles, embedded
/// graphics descriptors) — skipped from open tag to matching close.
fn is_raw_tag(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "template" | "noscript" | "textarea" | "svg"
    )
}

/// Decode one character reference at the start of `s` (which begins with
/// `&`), returning the decoded text and the bytes consumed. Anything that
/// is not a well-formed reference consumes just the `&`, so stray
/// ampersands and unknown names survive verbatim as page text.
fn decode_entity(s: &str) -> (String, usize) {
    let bytes = s.as_bytes();
    let mut end = 1usize; // scan past '&', stop at ';'
    while end < bytes.len() && end < 33 {
        let c = bytes[end];
        if c == b';' {
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == b'#') {
            return ("&".into(), 1);
        }
        end += 1;
    }
    if end >= bytes.len() || bytes[end] != b';' || end == 1 {
        return ("&".into(), 1);
    }
    let name = &s[1..end];
    let consumed = end + 1; // '&' + name + ';'
    let decoded = if let Some(digits) = name.strip_prefix('#') {
        let code = if let Some(hex) = digits.strip_prefix('x').or(digits.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()
        } else {
            digits.parse::<u32>().ok()
        };
        match code.and_then(char::from_u32) {
            Some(c) if c != '\0' && !(0xD800..=0xDFFF).contains(&(c as u32)) => c.to_string(),
            // An out-of-range reference is not ours to interpret: keep it
            // verbatim, the page said this literal.
            _ => return (s[..consumed].into(), consumed),
        }
    } else {
        match name {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => " ",
            "copy" => "©",
            "reg" => "®",
            "trade" => "™",
            "mdash" => "—",
            "ndash" => "–",
            "hellip" => "…",
            "middot" => "·",
            "lsquo" | "rsquo" => "'",
            "ldquo" | "rdquo" => "\"",
            "laquo" => "«",
            "raquo" => "»",
            "times" => "×",
            "divide" => "÷",
            "deg" => "°",
            "plusmn" => "±",
            "euro" => "€",
            "pound" => "£",
            "yen" => "¥",
            "cent" => "¢",
            "sect" => "§",
            "para" => "¶",
            "bull" => "•",
            "ensp" | "emsp" | "thinsp" => " ",
            _ => return (s[..consumed].into(), consumed),
        }
        .to_owned()
    };
    (decoded, consumed)
}

fn starts_with_ci(hay: &[u8], at: usize, needle: &[u8]) -> bool {
    hay.len() >= at + needle.len()
        && needle
            .iter()
            .enumerate()
            .all(|(k, n)| hay[at + k].eq_ignore_ascii_case(n))
}

fn find_byte(hay: &[u8], from: usize, byte: u8) -> Option<usize> {
    hay[from..].iter().position(|b| *b == byte).map(|p| from + p)
}

fn find_comment_end(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    loop {
        i = find_byte(b, i, b'-')?;
        if starts_with_ci(b, i, b"-->") {
            return Some(i + 3);
        }
        i += 1;
    }
}

/// Index of the `<` that closes a raw-text element, if one remains.
fn find_close_tag(text: &str, from: usize, tag: &str) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = from;
    loop {
        i = find_byte(b, i, b'<')?;
        if i + 1 < b.len()
            && b[i + 1] == b'/'
            && starts_with_ci(b, i + 2, tag.as_bytes())
            && matches!(b.get(i + 2 + tag.len()), Some(c) if *c == b'>' || c.is_ascii_whitespace())
        {
            return Some(i);
        }
        i += 1;
    }
}

/// Index just after the tag's closing `>`, honoring quoted attribute
/// values — a `>` inside `title="a>b"` must not end the tag.
fn skip_tag(b: &[u8], from: usize) -> usize {
    let mut quote: Option<u8> = None;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                quote = match quote {
                    Some(open) if open == q => None,
                    None => Some(q),
                    other => other,
                }
            }
            b'>' if quote.is_none() => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

// -------------------------------------------------------------------- pdf

/// PDF documents. Extraction failure is *reported*, never fabricated: a
/// scanned or corrupt PDF yields no chunks plus the reason, so the build
/// page can say "this PDF contributed nothing, because it has no text
/// layer" instead of silently missing a source.
pub struct PdfAdapter;

/// Shared PDF text extraction (was Tauri-only; core owns it now so CLI,
/// Wiki build and desktop all parse PDFs identically).
pub fn pdf_extract_text(path: &Path) -> Result<String> {
    pdf_extract::extract_text(path).map_err(|e| Error::Invalid(format!("PDF 解析失败：{e}")))
}

impl SourceAdapter for PdfAdapter {
    fn kind(&self) -> &'static str {
        "pdf"
    }

    fn supports(&self, path: &Path) -> bool {
        ext_is(path, &["pdf"])
    }

    fn extract(&self, path: &Path, max_bytes: usize) -> Result<SourceContent> {
        let name = display_name(path);
        let text = match pdf_extract::extract_text(path) {
            Ok(t) => t,
            Err(e) => {
                return Ok(SourceContent {
                    source_kind: self.kind().into(),
                    chunks: Vec::new(),
                    report: ParseReport {
                        skipped: vec![format!("PDF 解析失败：{e}")],
                        ..Default::default()
                    },
                })
            }
        };
        let mut report = ParseReport::default();
        let text = text.trim();
        if text.is_empty() {
            report
                .skipped
                .push(format!("{name} 未提取到文本（可能是扫描版 PDF）"));
            return Ok(SourceContent {
                source_kind: self.kind().into(),
                chunks: Vec::new(),
                report,
            });
        }
        let capped = text.len() > max_bytes;
        let cap = max_bytes.min(text.len());
        let head = &text[..floor_char_boundary(&text.as_bytes()[..cap])];
        if capped {
            report.truncated = true;
            report
                .skipped
                .push(format!("{name} 超出字节预算，仅提取前 {max_bytes} 字节"));
        }
        report.ok_bytes = head.len() as u64;
        Ok(SourceContent {
            source_kind: self.kind().into(),
            chunks: vec![ContentChunk {
                locator: format!("{name}#pdf"),
                text: head.to_owned(),
            }],
            report,
        })
    }
}

// ----------------------------------------------------------------- dir tree

/// A directory imported as a source becomes its *tree listing*: names and
/// nesting, not contents. This is the honest shape for "add a folder to
/// my knowledge space" — the structure is real knowledge (what exists,
/// how it is grouped), while file bodies stay separate sources.
pub struct DirTreeAdapter;

/// Tree walk limits: depth and total entries, so a huge import degrades
/// into a named truncation instead of an unbounded listing.
const TREE_MAX_DEPTH: usize = 6;
const TREE_MAX_ENTRIES: usize = 800;

impl SourceAdapter for DirTreeAdapter {
    fn kind(&self) -> &'static str {
        "dir_tree"
    }

    fn supports(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn extract(&self, path: &Path, _max_bytes: usize) -> Result<SourceContent> {
        let name = display_name(path);
        let mut lines: Vec<String> = Vec::new();
        let mut report = ParseReport::default();
        let mut truncated = false;
        walk_tree(path, Path::new(""), 0, &mut lines, &mut truncated);
        if truncated {
            report.truncated = true;
            report.skipped.push(format!(
                "目录树过大，仅列出前 {TREE_MAX_ENTRIES} 项（深度上限 {TREE_MAX_DEPTH} 层）"
            ));
        }
        if lines.is_empty() {
            report.skipped.push(format!("{name} 目录为空"));
        }
        let body = lines.join("\n");
        report.ok_bytes = body.len() as u64;
        let chunks = if body.is_empty() {
            Vec::new()
        } else {
            vec![ContentChunk {
                locator: format!("{name}#tree"),
                text: body,
            }]
        };
        Ok(SourceContent {
            source_kind: self.kind().into(),
            chunks,
            report,
        })
    }
}

fn walk_tree(dir: &Path, rel: &Path, depth: usize, lines: &mut Vec<String>, truncated: &mut bool) {
    if *truncated || depth >= TREE_MAX_DEPTH {
        *truncated = true;
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    // Sorted names keep the listing deterministic across platforms.
    let mut names: Vec<(String, PathBuf, bool)> = Vec::new();
    for ent in rd.flatten() {
        let ok = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        names.push((ent.file_name().to_string_lossy().into_owned(), ent.path(), ok));
    }
    names.sort();
    for (file_name, path, is_dir) in names {
        if lines.len() >= TREE_MAX_ENTRIES {
            *truncated = true;
            return;
        }
        let shown = if rel.as_os_str().is_empty() {
            file_name.clone()
        } else {
            format!("{}/{file_name}", rel.to_string_lossy().replace('\\', "/"))
        };
        if is_dir {
            lines.push(format!("{shown}/"));
            walk_tree(&path, Path::new(&shown), depth + 1, lines, truncated);
        } else {
            lines.push(shown);
        }
    }
}

/// Read a source as text honoring only the byte budget (used by callers
/// that just need the body, mirroring the old Tauri file-parse helper).
pub fn read_capped_text(path: &Path, max_bytes: usize) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|e| Error::Invalid(format!("无法读取：{e}")))?;
    let mut buf = Vec::new();
    f.by_ref()
        .take(max_bytes as u64)
        .read_to_end(&mut buf)
        .map_err(|e| Error::Invalid(format!("无法读取：{e}")))?;
    let head = floor_char_boundary(&buf);
    String::from_utf8(buf[..head].to_vec())
        .map(|s| s.trim_start_matches('\u{feff}').trim().to_owned())
        .map_err(|_| Error::Invalid("文本文件不是 UTF-8 编码".into()))
}
