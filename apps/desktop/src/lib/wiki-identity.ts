// Wiki 异步回写的身份闸门（纯逻辑，node --test 直接验证，见 tests/wiki-build-lifecycle.test.mjs）。
//
// 一切 Wiki 异步回写都必须在【请求发起时】领取一个身份三元组
// (workspace, gen 项目代次, req 请求身份)，响应回来时对照当前三元组校验：
// - 换项目（bind 到新 workspace）⇒ 代次 +1：在途旧请求全体失去回写资格；
// - 同类（kind）新请求 ⇒ 旧请求失去该类的回写资格；
// - 构建收尾的状态/覆盖回写只能沿用构建开始时领到的身份，
//   绝不允许在响应到达时重新申请序号"补票"——晚到的响应不能越过用户
//   之后的新动作重新获得回写资格（晚到 ≠ 最新）。
// 旧的裸序号守卫（收尾 ++seq）正是这样被绕过的：换项目后旧构建的收尾
// 再 ++ 就又"最新"了，于是把旧项目的状态盖到新项目头上。
type MaybePromise<T> = T | Promise<T>;

export type WikiIdentity = {
  /** 领取时绑定的项目（workspace 原样字符串）。 */
  workspace: string;
  /** 项目代次：每次换回写目标 +1，永不复用。 */
  gen: number;
  /** 请求身份：全局单调递增的领取序号。 */
  req: number;
  /** 回写类别（"build" / "status" / "coverage" / "evidence" …）。 */
  kind: string;
};

export interface WikiGate {
  /** 声明当前回写目标。同一目标重复 bind 不升代次；换目标即升代次。 */
  bind(workspace: string): void;
  /** 请求发起时领取身份；隐式 bind（用户对哪个项目发起动作，目标就是谁）。 */
  claim(workspace: string, kind?: string): WikiIdentity;
  /** 三元组齐合（目标 + 代次 + 该类最新请求）才放行回写。 */
  isCurrent(identity: WikiIdentity): boolean;
  /** 当前目标，供断言与调试。 */
  target(): { workspace: string; gen: number };
}

export function createWikiGate(): WikiGate {
  let workspace = "";
  let gen = 0;
  let req = 0;
  const latest = new Map<string, number>();
  const rebind = (ws: string) => {
    if (ws !== workspace) {
      workspace = ws;
      gen += 1;
    }
  };
  return {
    bind: rebind,
    claim(ws: string, kind = "default"): WikiIdentity {
      rebind(ws);
      const id: WikiIdentity = { workspace: ws, gen, req: ++req, kind };
      latest.set(kind, id.req);
      return id;
    },
    isCurrent(id: WikiIdentity): boolean {
      return id.workspace === workspace && id.gen === gen && latest.get(id.kind) === id.req;
    },
    target: () => ({ workspace, gen }),
  };
}

/** 一次 Wiki 构建动作的全部副作用出口（session.ts 提供实现）。 */
export interface WikiBuildOps<R, S, C> {
  /** 深度模型构建：core 全成或全败，resolve 出构建报告。 */
  build(): MaybePromise<R>;
  /** 确定性快速扫描（useModel=false 走这条路，不产生报告）。 */
  scan(): MaybePromise<void>;
  /** 收尾状态刷新。 */
  status(): MaybePromise<S>;
  /** 收尾覆盖刷新。 */
  coverage(): MaybePromise<C>;
  /** 以下写入只在身份仍有效时被调用。 */
  writeReport(report: R): void;
  writeError(error: unknown): void;
  writeStatus(status: S): void;
  writeCoverage(coverage: C): void;
}

/**
 * 构建动作的编排：开头一次性领取 build/status/coverage 三个身份，
 * 全流程（含 finally 的收尾回写）只校验这些身份。收尾不 claim——
 * 构建期间用户刷新过 status，就轮到用户的刷新结果落盘，旧构建的
 * 晚到响应作废；换项目后本构建全体回写作废。
 */
export async function runWikiBuild<R, S, C>(
  gate: WikiGate,
  workspace: string,
  useModel: boolean,
  ops: WikiBuildOps<R, S, C>,
): Promise<void> {
  const buildId = gate.claim(workspace, "build");
  const statusId = gate.claim(workspace, "status");
  const coverageId = gate.claim(workspace, "coverage");
  try {
    if (useModel) {
      const report = await ops.build();
      if (gate.isCurrent(buildId)) ops.writeReport(report);
    } else {
      await ops.scan();
    }
  } catch (err) {
    // 旧构建（已作废身份）的失败同样不许盖到新项目头上。
    if (gate.isCurrent(buildId)) ops.writeError(err);
  } finally {
    // 状态与覆盖各自独立成败：状态接口抖动不能连坐覆盖刷新。
    try {
      const status = await ops.status();
      if (gate.isCurrent(statusId)) ops.writeStatus(status);
    } catch {
      /* 收尾刷新失败不伪造、不覆盖构建自身的错误（错误已由 writeError 处理） */
    }
    try {
      const coverage = await ops.coverage();
      if (gate.isCurrent(coverageId)) ops.writeCoverage(coverage);
    } catch { /* 同上 */ }
  }
}
