// Wiki 证据跳转的纯逻辑：QA 依据分流（raw 源码证据 vs Wiki 页内证据）、
// 文章正文里 `相对路径:Lx-Ly` 引用的可点击化、建议子仓库路径拼接。
// 与 Vue 解耦以便 node --test 直接验证（见 apps/desktop/tests/wiki-evidence.test.mjs）。
// 注意：这里只做「可追溯」——行号能打开≠结论已被事实证明，该提示由页面负责展示。
import type { WikiHit } from "./types";

export type EvidenceTarget =
  | { mode: "source"; rel: string; start: number | null; end: number | null }
  | { mode: "page"; page: string };

/**
 * QA 依据点击目标：core 标注 kind="raw" 的是原始源码证据（带 1 起始行号，
 * 应打开项目文件定位到行）；其余（含旧数据缺 kind）按 Wiki 页内证据处理，
 * 打开对应页面。绝不把 raw 证据当 Wiki 页打开，也绝不跨项目猜路径。
 */
export function evidenceTarget(hit: Pick<WikiHit, "page" | "kind" | "start" | "end">): EvidenceTarget {
  if (hit.kind === "raw") {
    return { mode: "source", rel: hit.page, start: hit.start ?? null, end: hit.end ?? null };
  }
  return { mode: "page", page: hit.page };
}

/**
 * 把根目录 + 建议子仓库拼成完整路径（用户显式点击后才切换，不静默换项目）。
 * 分隔符跟随当前项目路径的写法；suggestion 是 core 给出的相对目录名。
 */
export function joinWorkspacePath(workspace: string, suggestion: string): string {
  const sep = workspace.includes("\\") ? "\\" : "/";
  const base = workspace.replace(/[\\/]+$/, "");
  const rel = suggestion.replace(/^[\\/]+/, "").replace(/[\\/]+$/, "").replace(/[\\/]/g, sep);
  return rel ? `${base}${sep}${rel}` : base;
}

/** 引用整体：`路径:L12-L30`、`路径:12-30`、`路径:L12`。路径必须像文件名（有点号扩展名），前面不能粘连其它标识符。 */
const CITATION = /(^|[^A-Za-z0-9_:/\\.\-])([\w.\-\\/]*\.[A-Za-z0-9]{1,8}):L?(\d+)(?:\s*[-–—~]\s*L?(\d+))?/g;

function escapeAttr(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function buttonHtml(full: string, rel: string, start: string, end: string | undefined, titlePrefix = "查看源码证据"): string {
  const range = end ? `L${start}-L${end}` : `L${start}`;
  return (
    `<button type="button" class="wiki-cite" data-rel="${escapeAttr(rel)}" ` +
    `data-start="${start}"${end ? ` data-end="${end}"` : ""} ` +
    `title="${escapeAttr(titlePrefix)} ${escapeAttr(rel)}:${range}">${full}</button>`
  );
}

/** 只在纯文本上替换；HTML 标签（属性里可能长得像引用）原样保留。 */
function linkifyTextNodes(chunk: string, titlePrefix?: string): string {
  let out = "";
  let i = 0;
  while (i < chunk.length) {
    const lt = chunk.indexOf("<", i);
    const text = lt === -1 ? chunk.slice(i) : chunk.slice(i, lt);
    out += text.replace(CITATION, (_m, prefix: string, rel: string, start: string, end?: string) =>
      `${prefix}${buttonHtml(_m.slice(prefix.length), rel, start, end, titlePrefix)}`,
    );
    if (lt === -1) break;
    const gt = chunk.indexOf(">", lt);
    if (gt === -1) {
      out += chunk.slice(lt);
      break;
    }
    out += chunk.slice(lt, gt + 1);
    i = gt + 1;
  }
  return out;
}

/**
 * 对 renderMarkdown 的输出做后处理：把正文中的 `路径:Lx-Ly` 引用变成可点击
 * 按钮。`<pre>`/`<code>` 块内不动（示例代码里的路径不是跳转入口），标签与
 * 属性不动。渲染器不认识这个类，页面自己处理 .wiki-cite 点击。
 * `titlePrefix` 按宿主语境定制（项目 wiki 点向源码，知识空间点向冻结来源
 * 版本），默认保持项目 wiki 既有文案。
 */
export function linkifyCitations(html: string, titlePrefix = "查看源码证据"): string {
  return html
    .split(/(<(?:pre|code)\b[\s\S]*?<\/(?:pre|code)>)/gi)
    .map((seg, index) => (index % 2 === 1 ? seg : linkifyTextNodes(seg, titlePrefix)))
    .join("");
}

/** 从 .wiki-cite 的 dataset 还原跳转目标（页面委托点击用；DOMStringMap 可直接传入）。 */
export function citationTarget(dataset: { rel?: string | undefined; start?: string | undefined; end?: string | undefined }): EvidenceTarget | null {
  const rel = dataset.rel;
  if (!rel) return null;
  const start = dataset.start ? Number(dataset.start) : null;
  const end = dataset.end ? Number(dataset.end) : null;
  return { mode: "source", rel, start: Number.isFinite(start as number) ? start : null, end: Number.isFinite(end as number) ? end : null };
}
