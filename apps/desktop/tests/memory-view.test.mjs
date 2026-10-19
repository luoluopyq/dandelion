import assert from 'node:assert/strict';
import test from 'node:test';
import { projectMemoryView, memoryBytes, filterMemoryEvents, memorySourceSession, memorySourceLabel, resolveMemorySelection } from '../node_modules/.wiki-view-test/memory-view.js';
const event = (id, extra = {}) => ({ id, kind: 'fact', authority: 'user_confirmed', observedAt: '2026-10-17T10:00:00Z', payload: { content: '界面使用简体中文' }, ...extra });
test('编辑和忘记只改变当前投影，保留完整变更记录', () => {
  const original = event('a');
  const revised = event('b', { revisionOf: 'a', sourceRef: {kind:'user_revision'}, payload:{content:'界面使用中英双语'} });
  const retracted = event('c', {kind:'retraction',revisionOf:'b',payload:{reason:'user requested forget'}});
  const view = projectMemoryView([original,revised,retracted]);
  assert.equal(view.confirmed.length,0);
  assert.equal(view.history.length,3);
  assert.equal(view.history.find(row=>row.event.id==='c').content,'界面使用中英双语');
  assert.equal(view.history.find(row=>row.event.id==='c').action,'忘记记忆');
  assert.ok(view.history.every(row=>!row.current));
});
test('候选编辑确认后才进入已确认，来源仍指向原对话', () => {
  const candidate = event('a',{authority:'model_candidate',sourceRef:{kind:'agent_proposal',id:'session-123:proposal'}});
  const confirmed = event('b',{revisionOf:'a',sourceRef:{kind:'user_revision'}});
  assert.equal(projectMemoryView([candidate]).pending.length,1);
  assert.equal(projectMemoryView([candidate]).confirmed.length,0);
  const view = projectMemoryView([candidate,confirmed]);
  assert.deepEqual(view.confirmed.map(row=>row.id),['b']);
  assert.equal(view.pending.length,0);
  assert.equal(view.history.find(row=>row.event.id==='b').action,'编辑并确认');
  assert.equal(memorySourceSession(confirmed,[candidate,confirmed]),'session-123');
  assert.equal(memorySourceLabel(confirmed,[candidate,confirmed]),'助手提议');
});
test('忽略候选不会使它进入已确认',()=>{
 const a=event('a',{authority:'model_candidate'}), b=event('b',{kind:'retraction',revisionOf:'a'});
 const view=projectMemoryView([a,b]);
 assert.equal(view.pending.length,0);assert.equal(view.confirmed.length,0);
 assert.equal(view.history.find(row=>row.event.id==='b').action,'忽略提议');
});
test('搜索、类型筛选与选择在列表变化后保持一致',()=>{
 const a=event('a'),b=event('b',{kind:'preference',payload:{content:'Use concise replies'}});
 assert.deepEqual(filterMemoryEvents([a,b],'简体','fact').map(row=>row.id),['a']);
 assert.deepEqual(filterMemoryEvents([a,b],'CONCISE','preference').map(row=>row.id),['b']);
 assert.equal(resolveMemorySelection([b],'a'),'b');assert.equal(resolveMemorySelection([],'a'),'');
});
test('内容限制按 UTF8 字节计算，与保存接口一致',()=>{
 assert.equal(memoryBytes('  中文🙂  '),10);assert.equal(memoryBytes('中'.repeat(1334)),4002);
});
test('来源损坏或循环时不会生成非法会话链接或无限遍历',()=>{
 const a=event('a',{revisionOf:'b'}),b=event('b',{revisionOf:'a'});
 assert.equal(memorySourceSession(a,[a,b]),null);
 assert.equal(memorySourceSession(event('c',{sourceRef:{kind:'agent_proposal',id:'../bad:x'}}),[]),null);
});
