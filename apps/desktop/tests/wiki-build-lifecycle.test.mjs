// Wiki 异步回写生命周期回归（顺序可控）：驱动【生产代码】runWikiBuild/createWikiGate
// （session.ts compileWiki 收尾、WikiPanel 证据跳转共用同一套身份闸门），用手工
// resolve 的 Promise 精确控制"谁先返回、谁后返回"。核心验收：
// 一切回写只认请求发起时领取的 (workspace, 项目代次, 请求身份) 三元组；
// 构建收尾绝不重新申请序号——晚到的旧响应不能越过用户之后的新动作回写。
import assert from "node:assert/strict";
import test from "node:test";

import { createWikiGate, runWikiBuild } from "../node_modules/.wiki-view-test/wiki-identity.js";

function deferred() {
  let resolve, reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

/** 记录每次回写落到 ops 上的值，供"谁写入了什么"做精确断言。 */
function makeOps(over = {}) {
  const writes = { report: [], error: [], status: [], coverage: [] };
  return {
    writes,
    build: () => (over.build ? over.build() : Promise.resolve("report")),
    scan: () => (over.scan ? over.scan() : Promise.resolve()),
    status: () => (over.status ? over.status() : Promise.resolve("closing-status")),
    coverage: () => (over.coverage ? over.coverage() : Promise.resolve("closing-coverage")),
    writeReport: (r) => writes.report.push(r),
    writeError: (e) => writes.error.push(String(e)),
    writeStatus: (s) => writes.status.push(s),
    writeCoverage: (c) => writes.coverage.push(c),
  };
}

test("身份三元组闸门：换目标升代次、同类新请求作废旧请求、跨类互不牵连", () => {
  const gate = createWikiGate();
  const a1 = gate.claim("A", "evidence");
  assert.ok(gate.isCurrent(a1), "领取即有效");
  assert.ok(gate.isCurrent(gate.claim("A", "status")), "不同 kind 各自最新，互不作废");
  const a2 = gate.claim("A", "evidence");
  assert.ok(!gate.isCurrent(a1) && gate.isCurrent(a2), "同类后来者作废先来者（连点场景）");
  const genAfterA = gate.target().gen;
  gate.bind("A");
  assert.equal(gate.target().gen, genAfterA, "重复 bind 同一目标不升代次");
  gate.bind("B");
  assert.ok(!gate.isCurrent(a2), "换目标：旧代次全体作废（claim 隐式 bind，切走后不得再领旧项目的号）");
  const back = gate.claim("A", "evidence");
  assert.ok(gate.isCurrent(back) && !gate.isCurrent(a1), "切回 A 也是新代次：A 的老请求依旧无效");
});

test("构建全程有效：报告与收尾状态/覆盖按身份顺序回写", async () => {
  const gate = createWikiGate();
  const ops = makeOps();
  await runWikiBuild(gate, "A", true, ops);
  assert.deepEqual(ops.writes, {
    report: ["report"], error: [], status: ["closing-status"], coverage: ["closing-coverage"],
  }, "无干扰时全部回写，且各写一次");
});

test("构建在途切项目：晚到的报告/错误/收尾状态/覆盖一律作废（旧 seq 守卫在这里会被绕开）", async () => {
  const gate = createWikiGate();
  const buildP = deferred();
  const closingStatus = deferred();
  const closingCoverage = deferred();
  const ops = makeOps({ build: () => buildP.promise, status: () => closingStatus.promise, coverage: () => closingCoverage.promise });
  const run = runWikiBuild(gate, "A", true, ops);
  // 构建还没回，用户切到 B 并对 B 刷新（refreshWiki 的领取动作）：代次升级。
  gate.claim("B", "status");
  assert.equal(gate.target().workspace, "B");
  buildP.resolve("late-report");
  await new Promise((r) => setImmediate(r));
  closingStatus.resolve("late-status");
  closingCoverage.resolve("late-coverage");
  await run;
  assert.deepEqual(ops.writes, { report: [], error: [], status: [], coverage: [] },
    "A 的构建晚到得再完整，也没有一个字能写进 B 的界面");
});

test("收尾不重新领号：构建期间用户刷新过 status，晚到的收尾状态必须让位", async () => {
  const gate = createWikiGate();
  const buildP = deferred();
  const closingStatus = deferred();
  const ops = makeOps({ build: () => buildP.promise, status: () => closingStatus.promise, coverage: () => Promise.resolve("cov") });
  const run = runWikiBuild(gate, "A", true, ops);
  // 同一项目，用户点了刷新：领了更新的 status 身份并先拿到了自己的结果。
  const refreshId = gate.claim("A", "status");
  const userWrites = [];
  const refreshStatus = "user-refresh-status";
  if (gate.isCurrent(refreshId)) userWrites.push(refreshStatus); // 模拟 refreshWiki 的守卫回写
  // 构建收尾的 status 响应最后到达——旧实现会在此刻 ++seq 重新占先，把
  // 构建中途的过期状态盖回界面；新实现只认构建开始时领的身份，已非最新。
  buildP.resolve("report");
  await new Promise((r) => setImmediate(r));
  closingStatus.resolve("stale-closing-status");
  await run;
  assert.deepEqual(userWrites, [refreshStatus], "用户的刷新结果落了盘");
  assert.deepEqual(ops.writes.status, [], "收尾晚到的旧状态被身份闸拦下：没有重新领号，就没有回写资格");
  assert.deepEqual(ops.writes.report, ["report"], "同一构建里仍属最新 kind 的报告照常回写");
});

test("同项目连续两次构建：第一次晚到的报告作废，第二次照常回写", async () => {
  const gate = createWikiGate();
  const first = deferred();
  const ops1 = makeOps({ build: () => first.promise });
  const run1 = runWikiBuild(gate, "A", true, ops1);
  const ops2 = makeOps();
  const run2 = runWikiBuild(gate, "A", true, ops2); // 用户再次点击构建
  first.resolve("stale-report");
  await Promise.all([run1, run2]);
  assert.deepEqual(ops1.writes.report, [], "被取代的旧构建报告不落盘");
  assert.deepEqual(ops2.writes.report, ["report"]);
});

test("构建失败：仍属当前身份才展示错误；换项目后的失败静默作废", async () => {
  const gate = createWikiGate();
  const opsOk = makeOps({ build: () => Promise.reject(new Error("模型未生成可见答复")) });
  await runWikiBuild(gate, "A", true, opsOk);
  assert.deepEqual(opsOk.writes.error, ["Error: 模型未生成可见答复"], "当前构建的错误照常展示，不掩盖");
  const failP = deferred();
  const opsLate = makeOps({ build: () => failP.promise });
  const run = runWikiBuild(gate, "A", true, opsLate);
  gate.bind("B");
  failP.reject(new Error("旧项目取消"));
  await run;
  assert.deepEqual(opsLate.writes.error, [], "换项目后的旧失败不写新项目错误");
});

test("快速扫描（useModel=false）：不产生报告，错误与收尾仍走同一身份闸", async () => {
  const gate = createWikiGate();
  const ops = makeOps();
  await runWikiBuild(gate, "A", false, ops);
  assert.deepEqual(ops.writes, { report: [], error: [], status: ["closing-status"], coverage: ["closing-coverage"] });
  const failP = deferred();
  const opsLate = makeOps({ scan: () => failP.promise });
  const run = runWikiBuild(gate, "A", false, opsLate);
  gate.bind("B");
  failP.reject(new Error("扫描被取代"));
  await run;
  assert.deepEqual(opsLate.writes.error, []);
});

test("收尾状态接口自身失败：不抛穿、不覆盖、覆盖刷新照旧", async () => {
  const gate = createWikiGate();
  const ops = makeOps({ status: () => Promise.reject(new Error("status 接口抖动")) });
  await runWikiBuild(gate, "A", true, ops);
  assert.deepEqual(ops.writes.report, ["report"]);
  assert.deepEqual(ops.writes.error, [], "收尾刷新失败不伪造构建错误");
  assert.deepEqual(ops.writes.coverage, ["closing-coverage"]);
});
