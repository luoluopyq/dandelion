//! C12: `web_search` / `web_extract`. Same registry as G1 tools. Failure is `Error:`.
//!
//! Search hits come from `dandelion.web.searchUrl` (JSON or DuckDuckGo-style HTML).
//! Extract is a direct HTTP GET + HTML-to-text. Not Firecrawl/browser.

use super::{Tool, ToolCtx};
use crate::acp::ToolKind;
use crate::jobs::truncate_bytes;
use crate::provider::types::{ToolSource, ToolSpec};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct WebSearchTool;
pub struct WebExtractTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebExtractTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebExtractTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web. Returns titles, URLs, and snippets. \
Use web_extract to read a specific page. Query operators such as site: may work \
when the configured search endpoint supports them."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (capped by settings maxResults)",
                        "minimum": 1
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return "Error: query required".into();
        }
        let cap = ctx.settings.web_max_results.max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, cap))
            .unwrap_or(cap);
        let url = search_url(&ctx.settings.web_search_url, query);
        let primary = fetch_text(&url, &ctx.settings, &ctx.cancel).await;
        let hits = primary
            .as_ref()
            .map(|body| parse_search_body(body, limit))
            .unwrap_or_default();
        if !hits.is_empty() {
            return json!({ "success": true, "data": { "web": hits } }).to_string();
        }
        // DuckDuckGo HTML can answer with a 202 challenge page containing no
        // result links. Try a public RSS search endpoint before sending the
        // model back to repeat the same failed search. Custom endpoints retain
        // their configured behavior.
        if ctx.settings.web_search_url.contains("duckduckgo.com") && !ctx.cancel.is_cancelled() {
            let fallback = search_url("https://www.bing.com/search?format=rss&q={query}", query);
            match fetch_text(&fallback, &ctx.settings, &ctx.cancel).await {
                Ok(body) => {
                    let hits = parse_search_body(&body, limit);
                    if !hits.is_empty() {
                        return json!({ "success": true, "data": { "web": hits } }).to_string();
                    }
                }
                Err(err) => {
                    return format!("Error: search sources unavailable (fallback: {err})");
                }
            }
        }
        match primary {
            Ok(_) => "Error: no search results from configured sources".into(),
            Err(err) => format!("Error: {err}"),
        }
    }
}

#[async_trait]
impl Tool for WebExtractTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_extract".into(),
            description: "Fetch web page URLs and return cleaned text (no LLM summary). \
HTTP(S) only. Failed URLs are listed with error; the loop continues."
                .into(),
            source: ToolSource::Builtin,
            parameters: json!({
                "type": "object",
                "properties": {
                    "urls": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "URLs to fetch"
                    },
                    "url": { "type": "string", "description": "Single URL if urls is omitted" }
                }
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    async fn call(&self, args: Value, ctx: ToolCtx) -> String {
        let mut urls = collect_urls(&args);
        if urls.is_empty() {
            return "Error: urls required".into();
        }
        let max_n = ctx.settings.web_max_extract_urls.max(1) as usize;
        if urls.len() > max_n {
            urls.truncate(max_n);
        }
        let max_bytes = ctx.settings.web_max_extract_bytes.max(1024);
        let mut results = Vec::new();
        for raw in urls {
            if ctx.cancel.is_cancelled() {
                return "Error: cancelled".into();
            }
            match allow_http_url(&raw) {
                Err(err) => results.push(json!({
                    "url": raw,
                    "title": "",
                    "content": "",
                    "error": err
                })),
                Ok(url) => match fetch_text(&url, &ctx.settings, &ctx.cancel).await {
                    Ok(body) => {
                        let text = html_to_text(&body);
                        let content = truncate_bytes(&text, max_bytes);
                        results.push(json!({
                            "url": url,
                            "title": "",
                            "content": content,
                            "error": Value::Null
                        }));
                    }
                    Err(err) => results.push(json!({
                        "url": url,
                        "title": "",
                        "content": "",
                        "error": err
                    })),
                },
            }
        }
        json!({ "results": results }).to_string()
    }
}

fn collect_urls(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = args.get("urls").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                out.push(s.to_string());
            } else if let Some(s) = item
                .get("url")
                .or_else(|| item.get("href"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                out.push(s.to_string());
            }
        }
    }
    if out.is_empty() {
        if let Some(s) = args
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push(s.to_string());
        }
    }
    out
}

pub fn allow_http_url(raw: &str) -> Result<String, String> {
    let t = raw.trim();
    let lower = t.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("only http(s) URLs are allowed".into());
    }
    Ok(t.to_string())
}

pub fn search_url(template: &str, query: &str) -> String {
    let enc = encode_query(query);
    if template.contains("{query}") {
        template.replace("{query}", &enc)
    } else if template.contains('?') {
        format!("{template}&q={enc}")
    } else {
        format!("{template}?q={enc}")
    }
}

fn encode_query(q: &str) -> String {
    let mut out = String::new();
    for b in q.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

async fn fetch_text(
    url: &str,
    settings: &crate::config::Settings,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let timeout = Duration::from_millis(settings.web_timeout_ms.max(1));
    let connect = Duration::from_millis(settings.http_connect_timeout_ms.max(1));
    let client = reqwest::Client::builder()
        .connect_timeout(connect)
        .timeout(timeout)
        .user_agent(settings.web_user_agent.clone())
        .build()
        .map_err(|err| err.to_string())?;
    let req = client.get(url).send();
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Err("cancelled".into()),
        res = req => res.map_err(|err| err.to_string())?,
    };
    let status = resp.status();
    let bytes = tokio::select! {
        _ = cancel.cancelled() => return Err("cancelled".into()),
        res = resp.bytes() => res.map_err(|err| err.to_string())?,
    };
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn parse_search_body(body: &str, limit: usize) -> Vec<Value> {
    let trimmed = body.trim();
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<rss") {
        let hits = parse_search_rss(trimmed, limit);
        if !hits.is_empty() {
            return hits;
        }
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(hits) = parse_search_json(trimmed, limit) {
            if !hits.is_empty() {
                return hits;
            }
        }
    }
    parse_search_html(body, limit)
}

fn parse_search_rss(body: &str, limit: usize) -> Vec<Value> {
    let items = Regex::new(r"(?is)<item>(.*?)</item>").expect("regex");
    let title = Regex::new(r"(?is)<title>(.*?)</title>").expect("regex");
    let link = Regex::new(r"(?is)<link>(.*?)</link>").expect("regex");
    let description = Regex::new(r"(?is)<description>(.*?)</description>").expect("regex");
    let mut out = Vec::new();
    for item in items.captures_iter(body).take(limit) {
        let item = item.get(1).map(|m| m.as_str()).unwrap_or("");
        let url = link
            .captures(item)
            .and_then(|m| m.get(1))
            .map(|m| m.as_str().trim().replace("&amp;", "&"))
            .unwrap_or_default();
        if allow_http_url(&url).is_err() {
            continue;
        }
        let heading = title
            .captures(item)
            .and_then(|m| m.get(1))
            .map(|m| html_to_text(m.as_str()))
            .unwrap_or_default();
        let snippet = description
            .captures(item)
            .and_then(|m| m.get(1))
            .map(|m| html_to_text(m.as_str()))
            .unwrap_or_default();
        if heading.is_empty() {
            continue;
        }
        out.push(json!({
            "title": heading,
            "url": url,
            "description": snippet,
            "position": out.len() + 1
        }));
    }
    out
}

fn parse_search_json(body: &str, limit: usize) -> Result<Vec<Value>, ()> {
    let v: Value = serde_json::from_str(body).map_err(|_| ())?;
    let arr = v
        .get("data")
        .and_then(|d| d.get("web"))
        .and_then(|w| w.as_array())
        .or_else(|| v.get("results").and_then(|r| r.as_array()))
        .or_else(|| v.as_array())
        .cloned()
        .ok_or(())?;
    let mut out = Vec::new();
    for (i, item) in arr.into_iter().enumerate() {
        if out.len() >= limit {
            break;
        }
        let url = item
            .get("url")
            .or_else(|| item.get("href"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if url.is_empty() {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or(url)
            .trim();
        let description = item
            .get("description")
            .or_else(|| item.get("content"))
            .or_else(|| item.get("snippet"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        out.push(json!({
            "title": title,
            "url": url,
            "description": description,
            "position": i + 1
        }));
    }
    Ok(out)
}

fn parse_search_html(body: &str, limit: usize) -> Vec<Value> {
    let re =
        Regex::new(r#"(?is)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#)
            .expect("regex");
    let strip = Regex::new(r"<[^>]+>").expect("regex");
    let mut out = Vec::new();
    for cap in re.captures_iter(body) {
        if out.len() >= limit {
            break;
        }
        let href = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let url = unwrap_ddg(href);
        if allow_http_url(&url).is_err() {
            continue;
        }
        let title_raw = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let title = strip.replace_all(title_raw, "").trim().to_string();
        if title.is_empty() {
            continue;
        }
        let pos = out.len() + 1;
        out.push(json!({
            "title": title,
            "url": url,
            "description": "",
            "position": pos
        }));
    }
    out
}

fn unwrap_ddg(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let rest = &href[idx + 5..];
        let enc = rest.split('&').next().unwrap_or(rest);
        return percent_decode(enc);
    }
    href.to_string()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn html_to_text(html: &str) -> String {
    let drop = Regex::new(
        r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<noscript[^>]*>.*?</noscript>",
    )
    .expect("regex");
    let tags = Regex::new(r"(?s)<[^>]+>").expect("regex");
    let ws = Regex::new(r"[ \t\f\v]+").expect("regex");
    let mut s = drop.replace_all(html, " ").into_owned();
    s = s.replace("&nbsp;", " ");
    s = s.replace("&amp;", "&");
    s = s.replace("&lt;", "<");
    s = s.replace("&gt;", ">");
    s = s.replace("&quot;", "\"");
    s = tags.replace_all(&s, " ").into_owned();
    let lines: Vec<String> = s
        .lines()
        .map(|l| ws.replace_all(l.trim(), " ").into_owned())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_results_parse() {
        let body = r#"{"results":[{"title":"Alpha","url":"https://a.example","content":"hi"}]}"#;
        let hits = parse_search_body(body, 8);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "Alpha");
    }

    #[test]
    fn bing_rss_results_parse() {
        let body = r#"<?xml version="1.0"?><rss><channel><item><title>AI &amp; Research</title><link>https://example.com/a</link><description>Latest &amp; verified</description></item></channel></rss>"#;
        let hits = parse_search_body(body, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["title"], "AI & Research");
        assert_eq!(hits[0]["url"], "https://example.com/a");
    }

    #[test]
    fn ddg_html_and_uddg() {
        let body = r#"<a class="result__a" href="https://example.com/a">Alpha Title</a>
<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fb">Beta</a>"#;
        let hits = parse_search_body(body, 8);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["url"], "https://example.com/a");
        assert_eq!(hits[1]["url"], "https://example.com/b");
    }

    #[test]
    fn strips_script_and_tags() {
        let t = html_to_text("<html><script>evil()</script><p>Hello &amp; world</p></html>");
        assert!(t.contains("Hello & world"), "{t}");
        assert!(!t.contains("evil"), "{t}");
    }

    #[test]
    fn refuses_file_url() {
        assert!(allow_http_url("file:///etc/passwd").is_err());
        assert!(allow_http_url("https://example.com").is_ok());
    }

    #[test]
    fn search_url_fills_placeholder() {
        let u = search_url("http://127.0.0.1/s?q={query}", "a b");
        assert!(u.contains("a%20b"), "{u}");
    }
}
