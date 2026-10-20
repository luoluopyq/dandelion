import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'vite';
import { createPinia, setActivePinia } from 'pinia';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';

// 工作单 2026-10-19 第二版·条目 7 钉死的前端契约：
// - 后台终端跑多久都不许把输出无界堆在前端内存里：rec.text 有上限，
//   且只留尾巴（截断标记唯一、恒在头部、重复调用不再啃尾巴）；
// - 任务栏预览只取最后一条非空行，且不许每秒把全部历史 split 一遍
//   （lib/terminal-preview.ts 从尾部倒扫 —— 这里用等价行为钉住它）。
//
// 每个用例独立起 vite + store，IPC 全部经 mockIPC。

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
    mockIPC(() => null, { shouldMockEvents: true });
    const pinia = createPinia();
    setActivePinia(pinia);
    const store = useSessionStore(pinia);
    await run({ store, server });
    store.$dispose();
  } finally {
    await server.close();
    clearMocks();
    globalThis.window = previous;
  }
}

const b64 = (s) => Buffer.from(s, 'utf8').toString('base64');

test('长时间终端输出：rec.text 封顶、只留尾巴、截断标记唯一且幂等', async () => {
  await withStore(async ({ store }) => {
    const sid = 's-term';
    const total = 300; // 300 × 1000 字符 ≈ 30 万，远超上限
    for (let i = 0; i < total; i += 1) {
      store.applyLiveUpdate(sid, {
        sessionUpdate: 'terminal_output_chunk',
        terminalId: 't1',
        data: b64(`第${i}行`.padEnd(1000, 'x') + '\n'),
      });
    }
    const rec = store.terminals['t1'];
    assert.ok(rec.text.length <= 128_000 + '…[早期输出已截断]\n'.length + 64,
      `输出必须封顶，实际 ${rec.text.length}`);
    assert.ok(rec.text.startsWith('…[早期输出已截断]\n'), '截断标记必须在头部');
    assert.equal(rec.text.indexOf('早期输出已截断', '…[早期输出已截断]\n'.length), -1, '标记不许出现第二份');
    assert.match(rec.text, /第299行/, '尾巴必须保住（最新输出优先）');
    assert.ok(!rec.text.includes('第0行'), '头部旧输出必须被挤掉');

    // 幂等：再喂一批小 chunk，只允许尾巴滚动，不许每轮再啃一截。
    const before = rec.text.length;
    store.applyLiveUpdate(sid, {
      sessionUpdate: 'terminal_output_chunk',
      terminalId: 't1',
      data: b64('tail\n'),
    });
    assert.ok(store.terminals['t1'].text.length <= before + 'tail\n'.length + 1);
  });
});

test('terminal_update 全量替换同样封顶', async () => {
  await withStore(async ({ store }) => {
    store.applyLiveUpdate('s-term', {
      sessionUpdate: 'terminal_update',
      terminalId: 't2',
      background: true,
      output: { data: b64('y'.repeat(500_000)) },
    });
    const rec = store.terminals['t2'];
    assert.ok(rec.text.length <= 128_000 + '…[早期输出已截断]\n'.length + 64);
    assert.ok(rec.text.startsWith('…[早期输出已截断]\n'));
  });
});

test('任务栏预览：只回最后一条非空行，行为与全量实现等价', async () => {
  const server = await createServer({
    optimizeDeps: { noDiscovery: true, include: [] },
    server: { middlewareMode: true, hmr: false, ws: false },
  });
  try {
    const { latestOutput } = await server.ssrLoadModule('/src/lib/terminal-preview.ts');
    const reference = (text) => {
      const lines = text.trimEnd().split(/\r?\n/).filter((l) => l.trim());
      return (lines[lines.length - 1] ?? '').trim() || '暂无输出';
    };
    assert.equal(latestOutput(''), '暂无输出');
    assert.equal(latestOutput('\n\n   \n'), '暂无输出');
    assert.equal(latestOutput('a\r\nb\r\n'), 'b');
    assert.equal(latestOutput('第一行\n\n   \n最后有用的行\n\n \n'), '最后有用的行');
    // 与参考实现随机等价：空白/换行的各种收尾。
    let seed = 7;
    const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
    const pieces = ['row\n', '\n', '  \n', 'tail line\n', 'x', '\r\n'];
    for (let i = 0; i < 200; i += 1) {
      const text = Array.from({ length: 8 }, () => pieces[Math.floor(rnd() * pieces.length)]).join('');
      assert.equal(latestOutput(text), reference(text), `分歧输入: ${JSON.stringify(text)}`);
    }
  } finally {
    await server.close();
  }
});
