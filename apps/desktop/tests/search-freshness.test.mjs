import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'vite';
import { createPinia, setActivePinia } from 'pinia';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';

// 工作单 2026-10-19 第二版·条目 6 的前端半边：搜索是异步的，
// 异步回来的结果只许更新「对应当前查询」的界面 —— 连续敲关键词时，
// 慢的旧查询后回来，不许把新查询的结果覆盖掉。
// （标题命中/多会话命中/限长摘要的口径在 store 层，由 Rust 测试钉死。）

async function withStore(run) {
  const previous = globalThis.window;
  const server = await createServer({
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, hmr: false, ws: false },
  });
  try {
    const { useSessionStore } = await server.ssrLoadModule('/src/stores/session.ts');
    globalThis.window = {
      innerWidth: 1200,
      crypto: globalThis.crypto,
      addEventListener() {},
      removeEventListener() {},
    };
    mockWindows('main');
    const calls = [];
    let handler = () => null;
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

const hit = (id, title) => ({ id, title, snippet: `${title} 的摘要`, updated_at: '2026-10-19T00:00:00+00:00' });

test('搜索乱序返回：过期查询的结果不得覆盖界面', async () => {
  await withStore(async ({ store, setHandler }) => {
    const resolvers = new Map();
    setHandler((command, args) => {
      if (command === 'sessions_search') {
        return new Promise((resolve) => resolvers.set(args.query, resolve));
      }
      return null;
    });

    const first = store.search('蒲'); // 慢查询先发出
    const second = store.search('蒲公英'); // 快查询后发出

    resolvers.get('蒲公英')([hit('s2', '蒲公英栽培')]);
    await second;
    assert.deepEqual(store.searchHits.map((h) => h.id), ['s2'], '新查询先回，正常上屏');

    resolvers.get('蒲')([hit('s1', '旧结果')]);
    await first;
    assert.deepEqual(store.searchHits.map((h) => h.id), ['s2'], '慢的旧查询后回来，不许覆盖');
  });
});

test('清空搜索框立刻清结果，未回来的旧查询不得把它填回去', async () => {
  await withStore(async ({ store, setHandler }) => {
    let resolveFirst;
    setHandler((command, args) => {
      if (command === 'sessions_search') {
        return new Promise((resolve) => { resolveFirst = resolve; });
      }
      return null;
    });
    const pending = store.search('关键词');
    await store.search(''); // 用户删空了输入框
    assert.deepEqual(store.searchHits, [], '清空立刻生效');
    resolveFirst([hit('s1', '迟到的旧命中')]);
    await pending;
    assert.deepEqual(store.searchHits, [], '迟到的旧结果必须被丢弃');
  });
});

test('搜索失败：报出原因、清空结果，不把异常吞了留旧列表', async () => {
  await withStore(async ({ store, setHandler }) => {
    setHandler((command) => {
      if (command === 'sessions_search') return Promise.reject(new Error('索引损坏'));
      return null;
    });
    await store.search('anything');
    assert.match(store.error, /索引损坏/);
    assert.deepEqual(store.searchHits, []);
  });
});
