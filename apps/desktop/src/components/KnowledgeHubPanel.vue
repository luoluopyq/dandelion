<template>
  <section class="knowledge-workspace">
    <aside class="knowledge-nav" aria-label="知识导航">
      <div class="nav-title">知识</div>
      <nav><button v-for="entry in entries" :key="entry.id" type="button" :class="{active: view === entry.id}" @click="navigate(entry.id)"><span>{{ entry.label }}</span><small v-if="entry.id === 'all'">{{ items.length }}</small></button></nav>
      <section class="nav-section"><h2>主题</h2><p v-if="!topics.length" class="nav-hint">保存内容后可自动归类，也能手动添加主题。</p><button v-for="topic in topics" :key="topic" type="button" :class="{active: view === 'topic' && selectedTopic === topic}" @click="navigate('topic',topic)"><span>{{ topic }}</span><small>{{ items.filter(i=>i.topics.includes(topic)).length }}</small></button></section>
      <details class="nav-projects" :open="view === 'project'"><summary>项目文档</summary><p class="nav-hint">为完整项目扫描和生成 Wiki。</p><button v-for="project in projectOptions" :key="project.workspace" type="button" :class="{active:view === 'project' && projectPath === project.workspace}" @click="navigate('project',project.workspace)">{{ project.name }}</button><button type="button" @click="addProject">＋ 添加项目文档</button></details>
      <button type="button" class="nav-manage" :class="{active:view === 'manage'}" @click="navigate('manage')">资料与权限管理</button>
    </aside>
    <main class="knowledge-main">
      <p v-if="projectError" role="alert">{{ projectError }}</p>
      <KnowledgeLibraryPanel v-if="view !== 'project' && view !== 'manage'" ref="library" :filter="view" :topic="selectedTopic" @metadata="items = $event" />
      <section v-else-if="view === 'project'" class="project-surface"><header><h1>项目文档</h1><p>根据真实项目文件整理 Wiki，保留源码出处与增量更新。</p></header><WikiPanel embedded hide-ask :project-path="projectPath" /></section>
      <section v-else class="management-surface"><header><h1>资料与权限管理</h1><p>维护已有资料库、文章及模型使用策略。日常收集在“全部知识”完成。</p></header><label class="scope"><span>资料库</span><select v-model="spaceId" aria-label="管理资料库"><option value="" disabled>选择资料库</option><option v-for="space in spaces" :key="space.id" :value="space.id">{{ space.name }}</option></select></label><SpaceView :space-id="spaceId" @space-change="spaceId = $event" @spaces-loaded="onSpacesLoaded" /></section>
    </main>
  </section>
</template>
<script setup lang="ts">
import { computed, ref } from 'vue';
import KnowledgeLibraryPanel from './KnowledgeLibraryPanel.vue';
import SpaceView from './SpaceView.vue';
import WikiPanel from './WikiPanel.vue';
import { useSessionStore } from '../stores/session';
import type { KnowledgeItem } from '../lib/types';
import type { KnowledgeFilter } from '../lib/knowledge-items';
import { folderPick, projectCreate } from '../lib/ipc';
const store=useSessionStore();
const view=ref<KnowledgeFilter|'project'|'manage'>('all'),selectedTopic=ref(''),projectPath=ref(''),spaceId=ref('');
const items=ref<KnowledgeItem[]>([]),spaces=ref<{id:string;name:string}[]>([]),library=ref<InstanceType<typeof KnowledgeLibraryPanel>|null>(null);
const projectError=ref('');
const entries:{id:KnowledgeFilter;label:string}[]=[{id:'all',label:'全部知识'},{id:'recent',label:'最近保存'},{id:'pending',label:'待处理'},{id:'favorites',label:'收藏'}];
const topics=computed(()=>[...new Set(items.value.flatMap(i=>i.topics))].sort((a,b)=>a.localeCompare(b,'zh-CN')));
const projectOptions=computed(()=>{const rows=store.projects.map(p=>({name:p.name,workspace:p.workspace}));const current=store.activeWorkspace;if(current&&!rows.some(p=>p.workspace===current))rows.unshift({name:current.split(/[\\/]/).pop()||'当前项目',workspace:current});return rows;});
function navigate(next:typeof view.value,value=''){const apply=()=>{view.value=next;if(next==='topic')selectedTopic.value=value;if(next==='project')projectPath.value=value;};if(library.value)library.value.beforeLeave(apply);else apply();}
function addProject(){const choose=async()=>{try{projectError.value='';const path=await folderPick();if(!path)return;const project=await projectCreate(path);await store.refreshProjects();view.value='project';projectPath.value=project.workspace;}catch(e){projectError.value=String(e);}};if(library.value)library.value.beforeLeave(()=>void choose());else void choose();}
function onSpacesLoaded(rows:{id:string;name:string}[]){spaces.value=rows;if(!rows.some(s=>s.id===spaceId.value))spaceId.value=rows[0]?.id||'';}
</script>
<style scoped>
.knowledge-workspace{flex:1;min-width:0;min-height:0;display:flex;overflow:hidden;color:var(--text);background:var(--bg)}.knowledge-nav{width:200px;flex-shrink:0;display:flex;flex-direction:column;gap:6px;padding:28px 14px 18px;border-right:1px solid var(--border);background:var(--bg-2);overflow:auto}.nav-title{font-size:17px;font-weight:650;margin:0 12px 20px}.knowledge-nav nav{display:grid;gap:3px}.knowledge-nav button{display:flex;align-items:center;justify-content:space-between;gap:8px;width:100%;padding:10px 12px;border:0;border-radius:7px;background:transparent;color:var(--muted);font:inherit;font-size:13px;text-align:left;cursor:pointer;overflow-wrap:anywhere}.knowledge-nav button.active{background:color-mix(in srgb,var(--accent) 12%,transparent);color:var(--accent);font-weight:600}.knowledge-nav button:hover{background:var(--hover)}.knowledge-nav button small{font-size:11px;font-weight:400;color:var(--muted)}.nav-section{margin-top:24px}.nav-section h2,.nav-projects summary{font-size:11px;font-weight:600;color:var(--muted);margin:0 12px 10px}.nav-projects{margin-top:24px}.nav-projects summary{cursor:pointer;padding:4px 0}.nav-hint{font-size:11px;color:var(--muted);line-height:1.8;margin:8px 12px}.knowledge-nav .nav-manage{margin-top:auto;padding-top:16px;font-size:11px}.knowledge-main{flex:1;min-width:0;min-height:0;overflow:auto;background:var(--panel)}.project-surface,.management-surface{padding:28px}.project-surface>header h1,.management-surface>header h1{font-size:24px;margin:0 0 8px}.project-surface>header p,.management-surface>header p{font-size:13px;color:var(--muted);margin:0 0 28px;line-height:1.8}.scope{display:flex;gap:12px;align-items:center;font-size:13px;color:var(--muted)}.scope select{padding:8px 12px;max-width:100%;background:var(--panel);color:var(--text);border:1px solid var(--border);border-radius:6px}button:focus-visible,summary:focus-visible,select:focus-visible{outline:2px solid var(--accent);outline-offset:3px}@media(max-width:800px){.knowledge-nav{width:150px;padding:24px 8px 14px}}@media(max-width:550px){.knowledge-workspace{flex-direction:column}.knowledge-nav{width:auto;padding:10px;flex-direction:row;flex-wrap:wrap;gap:3px;border-right:0;border-bottom:1px solid var(--border);max-height:150px}.nav-title,.nav-hint,.nav-section h2{display:none}.knowledge-nav nav{display:flex}.knowledge-nav button{width:auto;padding:7px 9px;white-space:nowrap;font-size:12px}.nav-section,.nav-projects{margin:0}.nav-projects summary{margin:0;padding:7px;font-size:12px}.nav-section{display:flex;overflow:auto}.knowledge-nav .nav-manage{margin:0;padding:7px}.knowledge-main{min-height:0}.project-surface,.management-surface{padding:18px}}
</style>
