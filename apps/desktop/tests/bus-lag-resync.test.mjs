import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { createServer } from 'vite';
import { createPinia, setActivePinia } from 'pinia';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';

// 事件队列落后（Lagged）的前端契约（v0.0.1 修复工作单 2026-10-19）：
// - 后端转发订阅丢帧时发 "bus-lagged"（带 missed/total），前端绝不静默吞掉；
// - 重建以落库状态为准：已结束的会话按导出窗口重刷消息与运行态；
// - 还在跑的会话尾部有未落库直播内容，不许被持久化窗口覆盖；
// - 已结束而终端没收到退出帧 = 状态可能就是丢的那批，标「未确认」。

const emptyBindings = { skill: [], mcp: [], corpus: [], knowledge: [] };
const exportFor = (id, extra = {}) => ({
  id,
  title: `对话 ${id}`,
  kind: 'user',
  parentId: null,
  workspace: '',
  messages: [],
  totalMessages: 0,
  prefs: {},
  ...extra,
});

async function withStore(run) {
  const previous = globalThis.window;
  const server = await createServer({
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, hmr: false, ws: false },
  });
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
    mockIPC((command, args) => handler(command, args), { shouldMockEvents: true });
    const pinia = createPinia();
    setActivePinia(pinia);
    const store = useSessionStore(pinia);
    await run({ store, setHandler: (h) => { handler = h; } });
    store.$dispose();
  } finally {
    await server.close();
    clearMocks();
    globalThis.window = previous;
  }
}

/** 启动即打开 s1 的最小后端：导出内容由 exports 实时提供，可在中途换内容。 */
const bootS1 = (exports) => (command, args) => {
  if (command === 'sessions_list') return [{ id: 's1', kind: 'user', title: '对话 s1' }];
  if (command === 'projects_list') return [];
  if (command === 'session_detached_ids') return [];
  if (command === 'session_export_window') return exports[args.sessionId];
  if (command === 'session_prefs_get') return {};
  if (command === 'session_task_bindings') return emptyBindings;
  return null;
};

test('"bus-lagged" 事件已接线到重同步，不再是静默丢帧', () => {
  const src = readFileSync(new URL('../src/stores/session.ts', import.meta.url), 'utf8');
  assert.match(src, /listen<\{ missed: number; total: number \}>\("bus-lagged"/, '必须监听 bus-lagged');
  assert.match(src, /resyncAfterLag\(ev\.payload\)/, '收到 lag 必须触发重同步');
});

test('lag 后会话已结束：按落库状态重刷消息、运行态，终端标未确认', async () => {
  await withStore(async ({ store, setHandler }) => {
    const exports = { s1: exportFor('s1', { lastRun: { status: 'idle', stopReason: 'end_turn' } }) };
    setHandler(bootS1(exports));
    await store.boot();
    assert.equal(store.activeId, 's1');

    // 模拟 lag 前的界面残留：显示着运行态、一条终端没收到退出帧。
    store.runState = { s1: { running: true, queued: false, stopReason: null } };
    store.terminals = {
      t1: { sessionId: 's1', exited: false },
      t2: { sessionId: 'other', exited: false },
      t3: { sessionId: 's1', exited: true },
    };
    // lag 期间后端已把这一轮落库：导出窗口里现在有完整收尾消息。
    exports.s1 = exportFor('s1', {
      lastRun: { status: 'idle', stopReason: 'end_turn' },
      messages: [{ role: 'assistant', content: '落库完成的收尾' }],
      totalMessages: 1,
    });

    await store.resyncAfterLag({ missed: 3, total: 7 });

    assert.equal(store.notice, '任务事件太快，界面错过了 3 帧（累计 7），已从保存的状态重新同步。');
    const rec = store.records.s1;
    assert.equal(rec.messages.length, 1, '已结束的会话要按落库窗口重建消息');
    assert.equal(rec.messages[0].text, '落库完成的收尾');
    assert.equal(rec.historyRemaining, 0);
    assert.equal(store.runState.s1.running, false, '落库口径显示已结束，运行态必须翻回来');
    assert.equal(store.runState.s1.stopReason, 'end_turn');
    assert.equal(store.terminals.t1.unverified, true, '已结束而终端缺退出帧 = 可能就是丢的那批，标未确认');
    assert.equal(store.terminals.t2.unverified, undefined, '别的会话的终端不受牵连');
    assert.equal(store.terminals.t3.unverified, undefined, '已收到退出帧的不标');
  });
});

test('lag 后会话仍在运行：不覆盖直播尾部、不动运行态、不标终端', async () => {
  await withStore(async ({ store, setHandler }) => {
    const exports = { s1: exportFor('s1', { lastRun: { status: 'running', stopReason: null } }) };
    setHandler(bootS1(exports));
    await store.boot();
    assert.equal(store.activeId, 's1');

    store.runState = { s1: { running: true, queued: false, stopReason: null } };
    store.terminals = { t1: { sessionId: 's1', exited: false } };
    // 落库窗口此刻只有已持久化的部分，直播内容还没落库。
    exports.s1 = exportFor('s1', {
      lastRun: { status: 'running', stopReason: null },
      messages: [{ role: 'assistant', content: '已落库的旧内容' }],
      totalMessages: 1,
    });

    await store.resyncAfterLag({ missed: 2, total: 5 });

    assert.equal(store.notice, '任务事件太快，界面错过了 2 帧（累计 5），已从保存的状态重新同步。');
    const rec = store.records.s1;
    assert.equal(rec.messages.length, 0, '仍在运行的会话尾部有未落库直播内容，不许被持久化窗口覆盖');
    assert.equal(store.runState.s1.running, true, '落库口径仍是 running，不许误翻成结束');
    assert.equal(store.terminals.t1.unverified, undefined, '运行中的终端缺退出帧是正常的，不该标未确认');
  });
});
