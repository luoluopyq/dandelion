import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'vite';
import { createPinia, setActivePinia } from 'pinia';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';

// 会话切换工作单（2026-10-19）钉死的前端契约：
// - 快速连点 A→B，最后一次点击必须赢；晚回来的 A 归还占用，不抢回界面；
// - 读消息失败必须归还占用并把原因摆出来，不许泄漏；
// - 目录暂时访问不到的空间只提示、绝不删除（projects_prune_missing 不得再被调用）；
// - 打开会话分段落计时：点击 / 占用完成 / 消息返回 / 首帧渲染完成；
// - sessions_list 的 200 条窗口：页装满时不能把窗口外的旧标签页关掉。
//
// 每个用例独立起 vite + store，IPC 全部经 mockIPC，命令全量记录用于回归断言。

const emptyBindings = { skill: [], mcp: [], corpus: [], knowledge: [] };
const exportFor = (id) => ({
  id,
  title: `对话 ${id}`,
  kind: 'user',
  parentId: null,
  workspace: '',
  messages: [],
  totalMessages: 0,
  prefs: {},
});

async function withStore(run) {
  const previous = globalThis.window;
  const server = await createServer({
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, hmr: false, ws: false },
  });
  const calls = [];
  let handler = () => null;
  try {
    const { useSessionStore } = await server.ssrLoadModule('/src/stores/session.ts');
    globalThis.window = {
      innerWidth: 1200,
      crypto: globalThis.crypto,
      addEventListener() {},
      removeEventListener() {},
    };
    mockWindows('main');
    mockIPC((command, args) => {
      calls.push({ command, args });
      return handler(command, args);
    }, { shouldMockEvents: true });
    const pinia = createPinia();
    setActivePinia(pinia);
    const store = useSessionStore(pinia);
    await run({ store, calls, setHandler: (h) => { handler = h; } });
    store.$dispose();
  } finally {
    await server.close();
    clearMocks();
    globalThis.window = previous;
  }
}

const released = (calls, id) =>
  calls.some((c) => c.command === 'session_release' && c.args?.sessionId === id && c.args?.surface === 'main');

test('快速连点 A→B：最后一次点击赢，晚回来的 A 归还占用不抢界面', async () => {
  await withStore(async ({ store, calls, setHandler }) => {
    const occupyResolvers = new Map();
    const shipped = [];
    setHandler((command, args) => {
      if (command === 'session_occupy') {
        return new Promise((resolve) => occupyResolvers.set(args.sessionId, resolve));
      }
      if (command === 'session_export_window') return exportFor(args.sessionId);
      if (command === 'session_prefs_get') return {};
      if (command === 'session_task_bindings') return emptyBindings;
      if (command === 'boot_marks') {
        shipped.push(...args.marks);
        return null;
      }
      return null;
    });

    const openingA = store.tryOpenSession('A');
    assert.equal(store.pendingOpenId, 'A', '点击立刻挂上加载态，不等第一轮 IPC');
    const openingB = store.tryOpenSession('B');

    occupyResolvers.get('B')();
    const bOk = await openingB;
    assert.equal(bOk, true, '后点的 B 正常打开');
    assert.equal(store.activeId, 'B');
    assert.deepEqual(store.openIds, ['B']);

    // A 的 occupy 晚于 B 的全程才回来：界面已经是 B，A 必须放弃并归还占用。
    occupyResolvers.get('A')();
    const aOk = await openingA;
    assert.equal(aOk, false, '被取代的先点不得报告成功');
    assert.ok(released(calls, 'A'), '晚回来的 A 必须释放刚拿到的占用');
    assert.equal(store.activeId, 'B', 'A 不许把界面抢回去');
    assert.deepEqual(store.openIds, ['B']);
    assert.equal(store.pendingOpenId, '', '加载态不留残影');

    await new Promise((r) => setTimeout(r, 20)); // 等首帧回调（setTimeout 兜底）落计时并 flush
    const phases = shipped.filter((m) => m.detail === 'session=B').map((m) => m.phase);
    assert.deepEqual(
      phases,
      ['open:click', 'open:occupied', 'open:messages', 'open:painted'],
      '打开会话的分阶段计时必须齐全且按序',
    );
  });
});

test('读消息失败：归还占用、报出原因、不留半开标签', async () => {
  await withStore(async ({ store, calls, setHandler }) => {
    setHandler((command) => {
      if (command === 'session_export_window') return Promise.reject(new Error('存储损坏'));
      return null;
    });
    const ok = await store.tryOpenSession('A');
    assert.equal(ok, false, '加载失败不能报成功');
    assert.ok(released(calls, 'A'), '失败必须归还刚拿到的占用，否则重试永远报占用');
    assert.match(store.error, /打开对话失败：存储损坏/, '失败原因要摆到用户面前');
    assert.deepEqual(store.openIds, [], '失败的会话不进标签栏');
    assert.equal(store.pendingOpenId, '');
  });
});

test('目录暂时访问不到：只提示、保留数据，绝不调用批量删除', async () => {
  await withStore(async ({ store, calls, setHandler }) => {
    let rows = [{ id: 'p1', name: '移动盘项目', workspace: 'E:\\missing', accessible: false }];
    setHandler((command) => (command === 'projects_list' ? rows : null));

    await store.refreshProjects();
    await store.notifyInaccessibleProjects();
    assert.match(store.notice, /1 个工作空间的目录暂时访问不到（移动盘项目）/);
    assert.match(store.notice, /对话已原样保留/, '提示必须说清数据没动');
    assert.equal(store.projects.length, 1, '空间条目原样保留');

    // 接上磁盘后：无提示，也不再有任何删除动作。
    rows = [{ id: 'p1', name: '移动盘项目', workspace: 'E:\\missing', accessible: true }];
    store.notice = null;
    await store.refreshProjects();
    await store.notifyInaccessibleProjects();
    assert.equal(store.notice, null, '目录回来了就不该有提示');

    assert.ok(
      !calls.some((c) => c.command === 'projects_prune_missing'),
      '前端任何路径都不得再触发「目录缺失就批量删除」',
    );
  });
});

// ---- 启动健壮性（v0.0.1 修复工作单 2026-10-19） ----------------------------
// 可选清单（插件/Skills/MCP/资料库/知识库/沙箱）挂掉或卡住，都不许把
// 「明明能打开的会话」拖成空白页；候选会话打不开要逐个回退，全部被独立
// 窗口占用时落到有说明的空屏，而不是卡死或报天书错误。

const CATALOG_COMMANDS = new Set([
  'agent_plugins_list',
  'skills_list',
  'mcp_list',
  'corpus_list',
  'knowledge_local_spaces',
  'sandbox_roots',
]);

/** boot() 全程的最小应答：目录/会话数据照常，清单类命令由参数决定死活。 */
const bootHandlers = ({ sessions = [], catalog }) => (command, args) => {
  if (CATALOG_COMMANDS.has(command)) return catalog ? catalog(command) : null;
  if (command === 'sessions_list') return sessions;
  if (command === 'projects_list') return [];
  if (command === 'session_detached_ids') return [];
  if (command === 'session_export_window') return exportFor(args.sessionId);
  if (command === 'session_prefs_get') return {};
  if (command === 'session_task_bindings') return emptyBindings;
  return null;
};

test('可选清单全部报错：会话照常打开，只聚合成一条提示', async () => {
  await withStore(async ({ store, setHandler }) => {
    setHandler(
      bootHandlers({
        sessions: [{ id: 's1', kind: 'user', title: '对话 s1' }],
        catalog: () => Promise.reject(new Error('清单服务未就绪')),
      }),
    );
    await store.boot();
    assert.equal(store.activeId, 's1', '清单全挂也必须把能打开的会话开出来');
    assert.deepEqual(store.openIds, ['s1']);
    for (let i = 0; i < 100 && !store.notice; i++) await new Promise((r) => setTimeout(r, 5));
    assert.match(store.notice, /部分功能清单加载失败（.*Agent 插件.*Skills.*MCP 服务.*资料库.*知识库.*沙箱目录.*）/);
    assert.match(store.notice, /不影响对话使用/, '提示要说清会话不受影响');
    assert.equal(store.error, null, '清单失败不该顶掉错误区');
  });
});

test('可选清单永远不返回：不阻塞会话打开，boot() 不等它们', async () => {
  await withStore(async ({ store, setHandler }) => {
    setHandler(
      bootHandlers({
        sessions: [{ id: 's1', kind: 'user', title: '对话 s1' }],
        catalog: () => new Promise(() => {}), // 永不落定：旧实现把 boot 一起拖死
      }),
    );
    const guard = new Promise((_, reject) =>
      setTimeout(() => reject(new Error('boot 被未响应的清单卡住了')), 5000),
    );
    await Promise.race([store.boot(), guard]);
    assert.equal(store.activeId, 's1', '清单卡死时首屏会话必须已经可见');
    assert.deepEqual(store.openIds, ['s1']);
  });
});

test('首个候选被独立窗口占用：自动回退到下一个能打开的会话', async () => {
  await withStore(async ({ store, setHandler }) => {
    setHandler((command, args) => {
      if (command === 'session_occupy' && args.sessionId === 's1') {
        return Promise.reject(new Error('该任务已在独立窗口打开（window-09:s1）'));
      }
      return bootHandlers({
        sessions: [
          { id: 's1', kind: 'user', title: '被占用的会话' },
          { id: 's2', kind: 'user', title: '可打开的会话' },
        ],
      })(command, args);
    });
    await store.boot();
    assert.equal(store.activeId, 's2', '第一个打不开就继续试下一个，不许停在空白页');
    assert.deepEqual(store.openIds, ['s2']);
    assert.ok(store.detachedIds.includes('s1'), '被占用的会话要标进独立窗口列表');
  });
});

test('普通会话全被独立窗口占用：落到有说明的空屏，boot 正常收尾', async () => {
  await withStore(async ({ store, setHandler }) => {
    setHandler((command, args) => {
      if (command === 'session_occupy') {
        return Promise.reject(new Error(`该任务已在独立窗口打开（window-09:${args.sessionId}）`));
      }
      return bootHandlers({
        sessions: [
          { id: 's1', kind: 'user', title: '会话 s1' },
          { id: 's2', kind: 'user', title: '会话 s2' },
        ],
      })(command, args);
    });
    await store.boot();
    assert.equal(store.activeId, '', '全部占用时停在新任务空屏，这是有意状态');
    assert.deepEqual(store.openIds, []);
    assert.match(store.error, /无法在主窗打开：该任务已在独立窗口占用/, '空屏必须给得出人话原因');
    assert.deepEqual(store.detachedIds, ['s1', 's2'], '两条都要标进独立窗口列表');
  });
});

test('sessions_list 装满 200 条时不关窗口外的旧标签，没装满时该关才关', async () => {
  await withStore(async ({ store, setHandler }) => {
    const page = Array.from({ length: 200 }, (_, i) => ({ id: `s${i}`, kind: 'user', title: `会话 ${i}` }));
    setHandler((command) => (command === 'sessions_list' ? page : null));
    store.openIds = ['old', 's5'];
    store.activeId = 'old';

    await store.refreshList();
    assert.deepEqual(store.openIds, ['old', 's5'], '页装满 200 条 = 可能被截断，窗口外的标签不能凭「不在页上」就关');
    assert.equal(store.activeId, 'old');

    // 页没装满：这一页就是全部，「不在页上」才等于真被删了。
    setHandler((command) =>
      command === 'sessions_list' ? [{ id: 's5', kind: 'user', title: '会话 5' }] : null,
    );
    await store.refreshList();
    assert.deepEqual(store.openIds, ['s5'], '真被删除的对话要关标签');
    assert.equal(store.activeId, 's5', '激活的标签被关后落到剩下的最后一个');
  });
});
