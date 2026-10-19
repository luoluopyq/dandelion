import test from 'node:test';
import assert from 'node:assert/strict';
import {filterKnowledgeItems,knowledgeStatus,parseTopics} from '../node_modules/.wiki-view-test/knowledge-items.js';
const base={id:'one',kind:'article',title:'Go',body:'context',notes:'',topics:['Go'],suggestedTopics:[],enabled:true,parseStatus:'ready',classificationStatus:'complete',favorite:false,updatedAt:'2026-10-18T00:00:00Z'};
test('pending processing does not hide saved content; topics and favorites are independent views',()=>{
 const pending={...base,id:'two',kind:'video',topics:[],parseStatus:'unsupported',classificationStatus:'pending',favorite:true};
 assert.equal(filterKnowledgeItems([base,pending],'all','','').length,2);
 assert.deepEqual(filterKnowledgeItems([base,pending],'pending','','').map(i=>i.id),['two']);
 assert.deepEqual(filterKnowledgeItems([base,pending],'topic','Go','').map(i=>i.id),['one']);
 assert.deepEqual(filterKnowledgeItems([base,pending],'favorites','','').map(i=>i.id),['two']);
 assert.equal(filterKnowledgeItems([base,pending],'all','','code').length,0);
 assert.equal(filterKnowledgeItems([base],'recent','','',Date.parse('2026-11-01')).length,0);
});
test('empty and unparsed attachments never claim full-text understanding',()=>{
 assert.match(knowledgeStatus({...base,parseStatus:'unsupported'}),/仅标题和笔记/);
 assert.match(knowledgeStatus({...base,parseStatus:'empty'}),/无正文/);
 assert.match(knowledgeStatus({...base,parseStatus:'failed'}),/原件已保存/);
 assert.match(knowledgeStatus({...base,enabled:false}),/尚未用于对话/);
 assert.deepEqual(parseTopics('Go，go, 并发编程、 并发编程\n软件工程'),['go','并发编程','软件工程']);
});
