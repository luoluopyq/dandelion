import DOMPurify from "dompurify";
import { marked, type Tokens } from "marked";

marked.setOptions({ gfm: true, breaks: false });

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

const renderer = new marked.Renderer();
renderer.code = ({ text, lang }: Tokens.Code) => {
  const language = lang ? ` class="language-${escapeHtml(lang)}"` : "";
  const payload = encodeURIComponent(text);
  return `<pre class="md-pre"><button type="button" class="copy-btn" data-copy="${payload}">复制</button><code${language}>${escapeHtml(text)}</code></pre>`;
};

/** Render one bubble. Do not re-parse the whole transcript on each chunk. */
export function renderMarkdown(source: string): string {
  const raw = marked.parse(source, { renderer, async: false }) as string;
  return DOMPurify.sanitize(raw, { USE_PROFILES: { html: true } });
}
