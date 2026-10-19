import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'vite';
test('external replies and visible final answers stay in the conversation',async()=>{
 const server=await createServer({optimizeDeps:{noDiscovery:true,include:[]},server:{middlewareMode:true,hmr:false,ws:false}});
 try{
  const {groupMessages}=await server.ssrLoadModule('/src/lib/group-messages.ts');
  const assistant={id:'answer',role:'assistant',text:'## Go 核心知识\n基础语法与并发',engineId:'claude_code'};
  const lateTool={id:'search',role:'tool',text:'knowledge_search',status:'completed'};
  const groups=groupMessages([{id:'u',role:'user',text:'整理知识'},assistant,lateTool]);
  assert.ok(groups.some(g=>g.type==='msg'&&g.msg.id==='answer'));
  const empty={id:'empty',role:'assistant',text:'',reasoningText:'private analysis'};
  assert.ok(groupMessages([assistant,empty]).some(g=>g.type==='msg'&&g.msg.id==='answer'));
  const final={...assistant,id:'final',text:'最终答复'};
  const native={...assistant,engineId:'native',text:'正在检索'};
  const nativeGroups=groupMessages([native,lateTool,final]);
  assert.ok(nativeGroups.some(g=>g.type==='thinking'&&g.items.some(m=>m.id==='answer')));
  assert.ok(nativeGroups.some(g=>g.type==='msg'&&g.msg.id==='final'));
 }finally{await server.close();}
});
