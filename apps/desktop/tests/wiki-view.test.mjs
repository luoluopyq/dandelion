// Wiki 阅读视图纯逻辑回归（npm test 先用 tsc 把 wiki-view.ts 编译到
// node_modules/.wiki-view-test，再由 node --test 运行本文件）。
// 覆盖验收清单里前端侧的行为：默认选页、切换项目、动态页面列表、
// 撤销后当前页回落、未构建/构建失败状态、按项目认领编译进度。
import assert from "node:assert/strict";
import test from "node:test";

import {
  buildFailureNotice,
  defaultPageFile,
  groupPages,
  pageProvenanceHint,
  readState,
  resolveSelectedPage,
  samePath,
} from "../node_modules/.wiki-view-test/wiki-view.js";

test("缺少来源与模型故障分开说明，首次构建不声称存在上一版文章", () => {
  const first = buildFailureNotice("sources", "目录为空", false);
  assert.match(first, /暂无可用于构建的项目文件/);
  assert.doesNotMatch(first, /上一/);
  assert.match(buildFailureNotice("evidence", "《主题地图》没有可用的证据文件", false), /本地知识库/);
  assert.match(buildFailureNotice("sources", "目录为空", true), /上一成功构建/);
  assert.match(buildFailureNotice("article", "网络中断", false), /网络中断/);
});

const page = (file, title = file, extra = {}) => ({ file, title, ...extra });
const CODE_PAGES = [
  page("ARCHITECTURE.md", "仓库构成", { role: "scan", group: "扫描索引" }),
  page("MODULES.md", "源码结构", { role: "scan", group: "扫描索引" }),
  page("OVERVIEW.md", "项目概览", { role: "overview", group: "概览" }),
];

test("默认打开注册表标记为 overview 的页面，而不是列表第一项", () => {
  assert.equal(defaultPageFile(CODE_PAGES), "OVERVIEW.md");
});

test("没有 overview 页时回落到注册表顺序第一页；空列表返回空串", () => {
  const scanOnly = CODE_PAGES.filter(p => p.role !== "overview");
  assert.equal(defaultPageFile(scanOnly), "ARCHITECTURE.md");
  assert.equal(defaultPageFile([]), "");
});

test("切换项目：列表整体更换且当前页不在其中时，选中新项目默认页", () => {
  const docsPages = [
    page("OVERVIEW.md", "项目概览", { role: "overview" }),
    page("OUTLINE.md", "资料大纲", { role: "scan" }),
  ];
  // 在代码项目里停留在 MODULES.md，切到资料项目后该页不存在 → 回落默认页
  assert.equal(resolveSelectedPage(docsPages, "MODULES.md"), "OVERVIEW.md");
  // 资料项目没有 overview 时落到第一页
  assert.equal(resolveSelectedPage(docsPages.slice(1), "MODULES.md"), "OUTLINE.md");
});

test("页面列表动态变化：当前页仍存在则保留（审核后新页不抢占）", () => {
  const withFlows = [...CODE_PAGES, page("FLOWS.md", "业务流程", { role: "distilled" })];
  assert.equal(resolveSelectedPage(withFlows, "MODULES.md"), "MODULES.md");
  // 撤销后页面被删 → 当前页消失，回落到默认页（“撤销后页面刷新”的选页前提）
  assert.equal(resolveSelectedPage(CODE_PAGES, "FLOWS.md"), "OVERVIEW.md");
});

test("目录分组完全按注册表元数据，未标注页面归入后备组", () => {
  const groups = groupPages([
    ...CODE_PAGES,
    page("FLOWS.md", "业务流程", { role: "distilled", group: "对话沉淀" }),
    page("EXTRA.md", "补充页"),
  ]);
  assert.deepEqual(
    groups.map(g => [g.group, g.pages.map(p => p.file)]),
    [
      ["扫描索引", ["ARCHITECTURE.md", "MODULES.md"]],
      ["概览", ["OVERVIEW.md"]],
      ["对话沉淀", ["FLOWS.md"]],
      ["其他", ["EXTRA.md"]],
    ],
  );
  assert.deepEqual(groupPages([]), []);
});

test("旧注册表（无 role/group）仍可选页：按注册表顺序取第一页", () => {
  const legacy = [page("ARCHITECTURE.md", "架构"), page("FEATURES.md", "功能")];
  assert.equal(resolveSelectedPage(legacy, ""), "ARCHITECTURE.md");
  assert.deepEqual(groupPages(legacy).map(g => g.group), ["其他"]);
});

test("阅读区状态机：未构建/构建中/失败/正常/空页互斥", () => {
  const base = { hasProject: true, hasCompiled: false, compiling: false, compileError: "", pageCount: 0 };
  assert.equal(readState(base), "unbuilt");
  assert.equal(readState({ ...base, hasProject: false }), "noProject");
  assert.equal(readState({ ...base, compiling: true }), "compiling");
  // 失败绝不显示成功：error 优先于已构建
  assert.equal(readState({ ...base, hasCompiled: true, compileError: "磁盘满" }), "error");
  assert.equal(readState({ ...base, hasCompiled: true, pageCount: 4 }), "reading");
  // 已构建但一个页面都没有（如全部被撤销）→ empty，而非假装在正常阅读
  assert.equal(readState({ ...base, hasCompiled: true, pageCount: 0 }), "empty");
});

test("编译进度按项目认领，路径写法差异不误认", () => {
  assert.ok(samePath("C:/Users/a/proj", "C:\\Users\\a\\proj"));
  assert.ok(samePath("C:\\Users\\a\\proj\\", "C:/Users/a/proj"));
  assert.ok(!samePath("C:\\Users\\a\\proj", "C:\\Users\\b\\proj"));
  // 事件缺 workspace 时不认领（调用方以空串比较失败处理）
  assert.ok(!samePath("", "C:\\Users\\a\\proj"));
});

test("来源提示按 role 给出，不靠文件名猜测", () => {
  assert.match(pageProvenanceHint(page("X.md", "X", { role: "scan" })), /快速扫描/);
  assert.match(pageProvenanceHint(page("X.md", "X", { role: "distilled" })), /对话沉淀/);
  assert.match(pageProvenanceHint(page("X.md", "X", { role: "overview" })), /模型/);
  assert.equal(pageProvenanceHint(page("X.md", "X")), null);
});
