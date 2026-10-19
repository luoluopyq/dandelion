// 知识空间异步回写回归（顺序可控）：驱动【生产代码】runSpaceRead/createSpaceGate
// （KnowledgeHubPanel 搜索/提问与 CustomerKnowledgePanel 检索/回答共用同一套闸门）。
// 核心验收：回写只认请求发起时领取的 (目标, 代次, 请求身份) 三元组；用户在途里
// 换空间/换类别后，旧响应与旧错误都不能再落盘——晚到 ≠ 最新。
import assert from "node:assert/strict";
import test from "node:test";

import { createSpaceGate, runSpaceRead, spaceTargetKey } from "../node_modules/.wiki-view-test/space-gate.js";

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

function makeOps(read) {
  const writes = { result: [], error: [] };
  return {
    reads: writes,
    ops: {
      read,
      write: result => writes.result.push(result),
      writeError: error => writes.error.push(error),
    },
  };
}

test("空间切换作废在途回写：旧库的晚到响应不落盘", async () => {
  const gate = createSpaceGate();
  const a = deferred(), b = deferred();
  const first = makeOps(() => a.promise);
  const second = makeOps(() => b.promise);
  const runA = runSpaceRead(gate, "库A", "search", first.ops);
  // 用户没等 A 返回就切到了 B 并再次检索。
  const runB = runSpaceRead(gate, "库B", "search", second.ops);
  a.resolve("A 的结果");
  await runA;
  assert.deepEqual(first.reads.result, [], "A 已作废，晚到响应不得回写");
  b.resolve("B 的结果");
  await runB;
  assert.deepEqual(second.reads.result, ["B 的结果"]);
});

test("旧请求的错误同样不许盖到新目标上", async () => {
  const gate = createSpaceGate();
  const a = deferred();
  const { reads, ops } = makeOps(() => a.promise);
  const runA = runSpaceRead(gate, "库A", "ask", ops);
  runSpaceRead(gate, "库B", "ask", makeOps(() => Promise.resolve("B")).ops);
  a.reject(new Error("A 失败"));
  await runA;
  assert.deepEqual(reads.error, [], "作废身份的失败必须静默作废");
});

test("同目标同类别：只有最新一次请求可回写；不同类别互不连坐", async () => {
  const gate = createSpaceGate();
  const slow = deferred(), fast = deferred();
  const first = makeOps(() => slow.promise);
  const second = makeOps(() => fast.promise);
  const runFirst = runSpaceRead(gate, "库A", "search", first.ops);
  const runSecond = runSpaceRead(gate, "库A", "search", second.ops);
  // 搜索与提问是不同 kind：各自都是本类最新，可并存回写。
  const ask = makeOps(() => Promise.resolve("答"));
  const runAsk = runSpaceRead(gate, "库A", "ask", ask.ops);
  fast.resolve("新搜索");
  await runSecond;
  assert.deepEqual(second.reads.result, ["新搜索"]);
  slow.resolve("旧搜索");
  await runFirst;
  assert.deepEqual(first.reads.result, [], "同类别被新请求顶替后不得回写");
  await runAsk;
  assert.deepEqual(ask.reads.result, ["答"], "ask 不受 search 顶替影响");
});

test("spaceTargetKey 规范化：同集合不同排列是同一目标，不误升代次", () => {
  const k1 = spaceTargetKey(["s2", "s1", "s2"]);
  const k2 = spaceTargetKey(["s1", "s2"]);
  assert.equal(k1, k2, "core 按集合语义解析，多选顺序挪动不该作废等价在途响应");
  const gate = createSpaceGate();
  gate.bind(k1);
  const gen = gate.target().gen;
  gate.bind(k2);
  assert.equal(gate.target().gen, gen, "同一集合重复 bind 不升代次");
  gate.bind(spaceTargetKey(["s3"]));
  assert.equal(gate.target().gen, gen + 1, "换集合才升代次");
});

test("身份三元组齐合才放行：手工构造的冒牌 id 一律拒绝", () => {
  const gate = createSpaceGate();
  const id = gate.claim("库A", "search");
  assert.ok(gate.isCurrent(id));
  assert.ok(!gate.isCurrent({ ...id, targetKey: "库B" }));
  assert.ok(!gate.isCurrent({ ...id, gen: id.gen + 1 }));
  assert.ok(!gate.isCurrent({ ...id, req: id.req + 1 }));
  assert.ok(!gate.isCurrent({ ...id, kind: "ask" }));
});
