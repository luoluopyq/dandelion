// Wiki 证据跳转纯逻辑回归（npm test 先用 tsc 编译 src/lib 下两个视图模块，
// 再由 node --test 运行）。覆盖：raw/wiki 依据分流、正文引用可点击化
// （pre/code 与 HTML 标签不受污染）、建议子仓库路径拼接、dataset 还原。
import assert from "node:assert/strict";
import test from "node:test";

import { citationTarget, evidenceTarget, joinWorkspacePath, linkifyCitations } from "../node_modules/.wiki-view-test/wiki-evidence.js";

test("kind=raw 的依据指向源码证据（带行号），其余按 Wiki 页处理", () => {
  assert.deepEqual(
    evidenceTarget({ page: "src/app.rs", kind: "raw", start: 10, end: 24 }),
    { mode: "source", rel: "src/app.rs", start: 10, end: 24 },
  );
  assert.deepEqual(evidenceTarget({ page: "OVERVIEW.md", kind: "wiki" }), { mode: "page", page: "OVERVIEW.md" });
  // 旧数据没有 kind 字段：按页处理，绝不按源码打开。
  assert.deepEqual(evidenceTarget({ page: "TOPIC-1.md" }), { mode: "page", page: "TOPIC-1.md" });
  // raw 但行号缺失（防御式）：仍是源码，行号给 null 由页面提示。
  assert.deepEqual(evidenceTarget({ page: "a/b.ts", kind: "raw" }), { mode: "source", rel: "a/b.ts", start: null, end: null });
});

test("建议子仓库拼接跟随当前路径的分隔符，且拒绝静默变成根目录", () => {
  assert.equal(joinWorkspacePath("E:\\example-project", "module"), "E:\\example-project\\module");
  assert.equal(joinWorkspacePath("E:/example-project/", "module"), "E:/example-project/module");
  assert.equal(joinWorkspacePath("E:\\a", "sub/dir"), "E:\\a\\sub\\dir");
  // 空建议不切换（返回项目根本身，调用方据此判断不启用按钮）。
  assert.equal(joinWorkspacePath("E:\\a", ""), "E:\\a");
});

test("正文引用变成按钮：路径、起止行进入 dataset，可见文本保持原样", () => {
  const html = "<p>入口在 crates/dandelion-core/src/runtime.rs:L4077-L4082，配置见 config.rs:88。</p>";
  const out = linkifyCitations(html);
  const m = out.match(/<button[^>]*data-rel="([^"]+)"[^>]*data-start="(\d+)"[^>]*data-end="(\d+)"[^>]*>([^<]*)<\/button>/);
  assert.ok(m, `未生成带行号的引用按钮：${out}`);
  assert.deepEqual([m[1], m[2], m[3]], ["crates/dandelion-core/src/runtime.rs", "4077", "4082"]);
  assert.equal(m[4], "crates/dandelion-core/src/runtime.rs:L4077-L4082");
  // 无 L 前缀、单行号也识别（没有 data-end）。
  assert.match(out, /data-rel="config\.rs" data-start="88"/);
});

test("pre/code 块与 HTML 标签内不做替换", () => {
  const html =
    '<pre><code>let x = "src/main.rs:L1-L2";</code></pre>' +
    '<p>看 <a href="https://ex.com/a.rs:L3-L4">链接</a> 和 src/main.rs:L1-L2</p>';
  const out = linkifyCitations(html);
  assert.ok(out.includes('<code>let x = "src/main.rs:L1-L2";</code>'), "代码块内容不得被改写");
  assert.ok(out.includes('href="https://ex.com/a.rs:L3-L4"'), "标签属性不得被改写");
  assert.ok(out.includes('class="wiki-cite" data-rel="src/main.rs"'), "正文中的引用应变成按钮");
});

test("非引用文本不误伤：时间、比例、无扩展名路径不生成按钮", () => {
  const html = "<p>比例 16:9，等待 3:05，目录 src/components 下</p>";
  assert.equal(linkifyCitations(html), html);
});

test("dataset 还原跳转目标；缺 rel 返回 null", () => {
  assert.deepEqual(citationTarget({ rel: "a/b.rs", start: "5", end: "9" }), { mode: "source", rel: "a/b.rs", start: 5, end: 9 });
  assert.deepEqual(citationTarget({ rel: "a/b.rs", start: "5", end: undefined }), { mode: "source", rel: "a/b.rs", start: 5, end: null });
  assert.equal(citationTarget({ rel: "", start: "1", end: undefined }), null);
});
