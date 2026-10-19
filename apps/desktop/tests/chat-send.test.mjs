import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'vite';
import { createSSRApp, h } from 'vue';
import { renderToString } from '@vue/server-renderer';
import { createPinia, setActivePinia } from 'pinia';
import { emit } from '@tauri-apps/api/event';
import { mockIPC, mockWindows, clearMocks } from '@tauri-apps/api/mocks';

// Render the actual ChatPane with its actual Pinia store. This guards the
// empty transcript boundary that originally swallowed preflight failures.
test('first-message startup and failure remain visible, and a retry can send', async () => {
  const previous = globalThis.window;
  const server = await createServer({ optimizeDeps: { noDiscovery: true, include: [] }, server: { middlewareMode: true, hmr: false, ws: false } });
  try {
    const { useSessionStore } = await server.ssrLoadModule('/src/stores/session.ts');
    const { default: ChatPane } = await server.ssrLoadModule('/src/components/ChatPane.vue');
    globalThis.window = { innerWidth: 1200, crypto: globalThis.crypto, addEventListener() {}, removeEventListener() {} };
    let rejectPrompt;
    let shouldFail = true;
    let bootstrapping = true;
    let releaseSnapshot;
    let delaySnapshot = true;
    let backgroundSnapshot = [];
    mockWindows('main');
    mockIPC((command) => {
      if (command === 'agent_engine_vision') return 'files';
      if (command === 'background_processes') return backgroundSnapshot;
      if (command === 'sessions_list') { if (bootstrapping) throw new Error('test: bus connected'); return [{ id: 'check', kind: 'user', title: '连接验证' }]; }
      if (command === 'session_export_window') return delaySnapshot
        ? new Promise(resolve => { releaseSnapshot = () => resolve({ messages: [], totalMessages: 0 }); })
        : { messages: [], totalMessages: 0 };
      if (command === 'session_prompt') return shouldFail
        ? new Promise((_, reject) => { rejectPrompt = reject; })
        : Promise.resolve('check');
      return null;
    }, { shouldMockEvents: true });
    const pinia = createPinia();
    setActivePinia(pinia);
    const store = useSessionStore(pinia);
    // Stop startup after the real event listeners are registered.
    await assert.rejects(store.boot(), /test: bus connected/);
    bootstrapping = false;
    store.info = { loopReady: true };
    store.agentPlugins = [{ id: 'claude_code', name: 'Claude Code', ready: true, installed: true }];
    store.records = { check: { id: 'check', title: '连接验证', kind: 'user', parentId: null, workspace: '', messages: [], historyRemaining: 0, loadingHistory: false, running: false, stopping: false, stopReason: null, prefs: { agentEngine: 'claude_code' }, skillIds: [], mcpIds: [], corpusIds: [], knowledgeIds: [] } };
    store.activeId = 'check';
    store.openIds = ['check'];
    const render = async () => (await renderToString(createSSRApp({ render: () => h(ChatPane) }).use(pinia))).replace(/<!--.*?-->/gs, "");
    assert.doesNotMatch(await render(), /role="alert"/);
    const terminalFrame = update => emit('session-update', { jsonrpc: '2.0', method: 'session/update', params: { sessionId: 'check', update } });
    await terminalFrame({ sessionUpdate: 'terminal_update', terminalId: 'proc_foreground', command: 'echo hi', background: false });
    assert.doesNotMatch(await render(), /后台运行 1 个/);
    await terminalFrame({ sessionUpdate: 'terminal_update', terminalId: 'proc_background', command: 'npm run dev', background: true });
    assert.match(await render(), /后台运行 1 个/);
    await terminalFrame({ sessionUpdate: 'terminal_update', terminalId: 'proc_background', exitStatus: { exitCode: 0 } });
    assert.match(await render(), /后台任务已结束/);
    backgroundSnapshot = [{ id: 'proc_restored', command: 'npm run dev', cwd: '/workspace', status: 'running', exitCode: null, elapsedMs: 2000, output: 'ready\n' }];
    await store.refreshBackgroundProcesses('check');
    assert.equal(store.terminals.proc_restored.text, 'ready\n');
    assert.match(await render(), /后台运行 1 个/);
    const send = store.send('你好');
    assert.equal(store.messages.at(-1)?.text, '你好', 'local echo appears before the first IPC completes');
    const starting = await render();
    assert.match(starting, /你好<\/pre>/);
    assert.ok(starting.indexOf('你好</pre>') < starting.indexOf('role="status"'), 'user bubble precedes generation status');
    assert.ok(releaseSnapshot);
    delaySnapshot = false;
    releaseSnapshot();
    for (let i = 0; i < 20 && !rejectPrompt; i++) await new Promise(setImmediate);
    assert.ok(rejectPrompt, 'real store must dispatch session_prompt');
    assert.match(await render(), /role="status"[^>]*>模型正在生成/);
    rejectPrompt('代理启动失败：连接被拒绝');
    assert.equal(await send, false);
    assert.equal(store.running, false);
    assert.equal(store.canSend, true);
    const failed = await render();
    assert.match(failed, /role="alert"[^>]*>代理启动失败：连接被拒绝/);
    assert.equal(store.messages.length, 0, 'rejected preflight must not create a phantom user message');
    shouldFail = false;
    assert.equal(await store.send('你好'), true);
    assert.equal(store.error, null, 'retry clears the previous error');
    assert.equal(store.messages.length, 1);
    const frame = update => emit('session-update', { jsonrpc: '2.0', method: 'session/update', params: { sessionId: 'check', update } });
    const ack = { sessionUpdate: 'user_message_chunk', messageId: 'server-user-1', content: { type: 'text', text: '你好' } };
    await frame(ack);
    await frame(ack);
    assert.equal(store.messages.length, 1, 'server acknowledgement and replay must not duplicate the local echo');
    assert.equal(store.messages[0].id, 'server-user-1');
    await frame({ sessionUpdate: 'state_update', state: 'idle', stopReason: 'end_turn' });
    shouldFail = true;
    rejectPrompt = undefined;
    const acceptedSend = store.send('再问一次');
    await frame({ ...ack, messageId: 'server-user-2', content: { type: 'text', text: '再问一次' } });
    for (let i = 0; i < 20 && !rejectPrompt; i++) await new Promise(setImmediate);
    assert.ok(rejectPrompt);
    rejectPrompt('执行失败');
    assert.equal(await acceptedSend, true, 'failure after acceptance retains the submitted message');
    assert.equal(store.messages.length, 2);
    assert.equal(store.failedAttempt.prompt, '再问一次');
    rejectPrompt = undefined;
    const rejectedRepeat = store.send('你好');
    for (let i = 0; i < 20 && !rejectPrompt; i++) await new Promise(setImmediate);
    assert.ok(rejectPrompt);
    rejectPrompt('再次连接失败');
    assert.equal(await rejectedRepeat, false, 'an older identical prompt is not acceptance of the new attempt');
    assert.equal(store.messages.length, 2, 'only the rejected local echo is removed');
    store.$dispose();
  } finally {
    await server.close();
    clearMocks();
    globalThis.window = previous;
  }
});
