// 知识空间异步回写的身份闸门（纯逻辑，node --test 直接验证，见 tests/space-gate.test.mjs）。
//
// createWikiGate 的空间版：知识页（Hub 搜索/提问、资料库检索/回答）的每一次
// 异步回写都必须在【请求发起时】领取身份三元组 (targetKey 空间目标, gen 目标
// 代次, req 请求身份)，响应回来时对照当前三元组校验：
// - 换目标（切换空间 / 切换资料库 / 从 Wiki 切到资料）⇒ 代次 +1，在途旧请求
//   全体失去回写资格；
// - 同类（kind："search" / "ask" …）新请求 ⇒ 同类旧请求失去回写资格；
// - 绝不允许在响应到达时重新领取序号"补票"——晚到的响应不能越过用户之后的
//   新动作重新获得回写资格（晚到 ≠ 最新）。
// Hub 此前的裸 await 没有这层守卫：慢响应会在用户换库后把旧库的结果盖上去。
type MaybePromise<T> = T | Promise<T>;

export type SpaceIdentity = {
  /** 领取时绑定的读取目标（spaceTargetKey 产出的规范化字符串）。 */
  targetKey: string;
  /** 目标代次：每次换读取目标 +1，永不复用。 */
  gen: number;
  /** 请求身份：全局单调递增的领取序号。 */
  req: number;
  /** 回写类别（"search" / "ask" …）。 */
  kind: string;
};

export interface SpaceGate {
  /** 声明当前读取目标。同一目标重复 bind 不升代次；换目标即升代次。 */
  bind(targetKey: string): void;
  /** 请求发起时领取身份；隐式 bind（用户对哪个空间发起动作，目标就是谁）。 */
  claim(targetKey: string, kind?: string): SpaceIdentity;
  /** 三元组齐合（目标 + 代次 + 该类最新请求）才放行回写。 */
  isCurrent(identity: SpaceIdentity): boolean;
  /** 当前目标，供断言与调试。 */
  target(): { targetKey: string; gen: number };
}

export function createSpaceGate(): SpaceGate {
  let targetKey = "";
  let gen = 0;
  let req = 0;
  const latest = new Map<string, number>();
  const rebind = (key: string) => {
    if (key !== targetKey) {
      targetKey = key;
      gen += 1;
    }
  };
  return {
    bind: rebind,
    claim(key: string, kind = "default"): SpaceIdentity {
      rebind(key);
      const id: SpaceIdentity = { targetKey: key, gen, req: ++req, kind };
      latest.set(kind, id.req);
      return id;
    },
    isCurrent(id: SpaceIdentity): boolean {
      return id.targetKey === targetKey && id.gen === gen && latest.get(id.kind) === id.req;
    },
    target: () => ({ targetKey, gen }),
  };
}

/**
 * 规范化多空间读取目标：去重、排序后拼接。core 的 space_search/space_ask
 * 按集合语义解析空间列表（顺序无关），所以同一集合的两种排列必须共享
 * 身份——否则用户挪一下多选顺序就会把在途的等价响应误判为过期。
 */
export function spaceTargetKey(spaceIds: string[]): string {
  return [...new Set(spaceIds)].sort().join("\n");
}

/** 一次空间读取（搜索或提问）的副作用出口（组件提供实现）。 */
export interface SpaceReadOps<R> {
  /** 发起读取（spaceSearch / spaceAsk / 兼容检索）。失败照常抛出。 */
  read(): MaybePromise<R>;
  /** 以下写入只在身份仍有效时被调用。 */
  write(result: R): void;
  writeError(error: unknown): void;
}

/**
 * 读取动作的编排：发起前领取身份，回写（含错误）只认这份身份。
 * 用户在途里换了空间 ⇒ 身份作废，旧响应与旧错误都不落盘。
 */
export async function runSpaceRead<R>(
  gate: SpaceGate,
  targetKey: string,
  kind: string,
  ops: SpaceReadOps<R>,
): Promise<void> {
  const id = gate.claim(targetKey, kind);
  try {
    const result = await ops.read();
    if (gate.isCurrent(id)) ops.write(result);
  } catch (err) {
    if (gate.isCurrent(id)) ops.writeError(err);
  }
}
