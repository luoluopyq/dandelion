// Wiki 阅读视图的纯逻辑：页面选择、目录分组、项目路径匹配。
// 与 Vue 解耦以便 node --test 直接验证（见 apps/desktop/tests/wiki-view.test.mjs）。
// 规则全部依赖后端注册表元数据（role/group），不认任何具体页面文件名。
import type { WikiPageEntry } from "./types";

/** 与后端 default_page_role 对应的稳定语义标记。 */
export const ROLE_OVERVIEW = "overview";

/** Explain source failures separately from provider failures, including old persisted errors. */
export function buildFailureNotice(stage: string, reason: string, hasPreviousBuild: boolean): string {
  const missingSources = stage === "sources" || (stage === "evidence" && reason.includes("没有可用的证据文件"));
  const message = missingSources
    ? "暂无可用于构建的项目文件。请检查项目目录和扫描覆盖；独立文章、代码片段等内容可保存在本地知识库，无需创建项目。"
    : `知识整理未完成（${stage}：${reason}）。`;
  return message + (hasPreviousBuild ? " 上一成功构建的文章仍可阅读。" : " 已保存的原始资料和知识条目未受影响。");
}

/** Windows 优先的路径比较：斜杠统一、去尾分隔符、忽略大小写。 */
export function samePath(a: string, b: string): boolean {
  const norm = (p: string) => p.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  return norm(a) === norm(b);
}

/**
 * 默认打开的页面：注册表中标为 overview 的页面优先（"项目概览"），
 * 没有则退回注册表顺序的第一个实际存在的页面。空列表返回 ""。
 */
export function defaultPageFile(pages: readonly WikiPageEntry[]): string {
  if (!pages.length) return "";
  return pages.find(page => page.role === ROLE_OVERVIEW)?.file ?? pages[0].file;
}

/**
 * 当前页在最新页面列表中仍存在就保留，否则落到默认页。
 * 覆盖：进入项目（无当前页）、切换项目（列表整体更换）、
 * 撤销导致页面被删（当前页消失）、审核后新页面出现（不抢占当前页）。
 */
export function resolveSelectedPage(
  pages: readonly WikiPageEntry[],
  current: string,
): string {
  if (current && pages.some(page => page.file === current)) return current;
  return defaultPageFile(pages);
}

export interface WikiTocGroup {
  group: string;
  pages: WikiPageEntry[];
}

/** 按注册表元数据分组；未标注的页面归入 fallback 组，组的顺序 = 首次出现顺序。 */
export function groupPages(pages: readonly WikiPageEntry[], fallback = "其他"): WikiTocGroup[] {
  const groups: WikiTocGroup[] = [];
  for (const page of pages) {
    const name = page.group || fallback;
    let bucket = groups.find(entry => entry.group === name);
    if (!bucket) {
      bucket = { group: name, pages: [] };
      groups.push(bucket);
    }
    bucket.pages.push(page);
  }
  return groups;
}

/** 正文区的来源提示：由 role 决定，不看文件名。返回 null 表示无需提示。 */
export function pageProvenanceHint(page: WikiPageEntry | null | undefined): string | null {
  if (!page) return null;
  switch (page.role) {
    case "scan":
      return "本页由快速扫描自动生成，只索引项目文件中确实存在的内容。";
    case "distilled":
      return "本页来自对话沉淀的知识条目，每条标注来源对话；只有审核通过的条目会出现在这里。";
    case ROLE_OVERVIEW:
      return "本页由模型阅读扫描结果后整理，非逐字原文。";
    default:
      return null;
  }
}

export type WikiReadState = "noProject" | "unbuilt" | "compiling" | "error" | "reading" | "empty";

/**
 * 阅读区状态机（单一出口，保证提示互斥、失败不伪装成成功）。
 * hasCompiled=未构建 → unbuilt（构建按钮）；compiling → 已保存正在更新；
 * error → 编译失败（条目保留，可重试）；有页 → reading；已构建但无页 → empty。
 */
export function readState(input: {
  hasProject: boolean;
  hasCompiled: boolean;
  compiling: boolean;
  compileError: string;
  pageCount: number;
}): WikiReadState {
  if (!input.hasProject) return "noProject";
  if (input.compiling) return "compiling";
  if (input.compileError) return "error";
  if (!input.hasCompiled) return "unbuilt";
  return input.pageCount > 0 ? "reading" : "empty";
}
