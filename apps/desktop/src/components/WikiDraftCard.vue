<template>
  <article class="wiki-draft" aria-label="对话知识保存">
    <header>
      <strong>{{ savedAlready ? '知识已保存' : '对话知识草稿' }}</strong>
      <span class="wiki-draft-kind">{{ KIND_LABELS[draft.kind] ?? draft.kind }}</span>

      <button type="button" class="wiki-draft-dismiss" title="忽略这条草稿（不创建任何东西）" @click="dismiss">忽略</button>
    </header>
    <p class="wiki-draft-note">{{ savedAlready ? '已保存为独立知识，无需创建项目。可在知识页面阅读、归类和检索。' : '直接保存到本地知识，也可以按需关联项目文档。' }}</p>
    <div class="wiki-draft-actions"><button v-if="!savedAlready" type="button" :disabled="busy" class="primary" @click="saveLocally">保存为独立知识</button><button type="button" @click="store.openKnowledge()">打开知识</button></div>
    <details><summary>查看待保存的知识</summary><blockquote class="wiki-draft-content">{{ draft.content }}</blockquote></details>

    <div v-if="done" class="wiki-draft-done" role="status">
      已完成：草稿提交为{{ done.status === "accepted" ? "已写入" : "待审核" }}条目，项目「{{ doneName }}」。{{ done.status === "pending" ? "可在项目 Wiki 页审核。" : "可在项目 Wiki 页查看。" }}
    </div>
    <template v-else>
      <details><summary>提交到已有项目（待审核）</summary><div class="wiki-draft-actions">
        <select v-model="pickedProject" :disabled="busy" aria-label="选择已登记项目" @change="chooseProject">
          <option value="" disabled>选择已登记项目…</option>
          <option v-for="project in store.projects" :key="project.id" :value="project.id">{{ project.name }}</option>
        </select>
        <button type="button" :disabled="busy" @click="chooseExistingDir">选择现有目录并登记…</button>
        <button type="button" :disabled="busy" @click="startNewProject">新建项目目录…</button>
      </div></details>
      <div v-if="naming" class="wiki-draft-new">
        <label>项目名称<input v-model="newName" :placeholder="nameLoading ? '模型正在生成项目名称…' : '项目名称，可修改'" :disabled="busy || nameLoading" @keydown.enter="confirmNewProject" /></label>
        <button type="button" :disabled="busy || nameLoading" @click="generateName">{{ nameLoading ? '正在生成名称…' : '生成项目名' }}</button>
        <p v-if="targetPath" class="wiki-draft-path" role="status">将创建：<code>{{ targetPath }}</code></p>
        <p v-else class="wiki-draft-path">先选择存放项目的父目录。确认后创建项目并正式写入 Wiki。</p>
        <div class="wiki-draft-actions">
          <button type="button" :disabled="busy || !newName.trim()" @click="pickParent">{{ parent ? "重选父目录…" : "选择父目录…" }}</button>
          <button type="button" class="primary" :disabled="busy || nameLoading || !newName.trim() || !parent" @click="confirmNewProject">创建项目并写入 Wiki</button>
          <button type="button" :disabled="busy" @click="naming = false">取消</button>
        </div>
      </div>
      <p v-if="error" class="wiki-draft-error" role="alert">{{ error }}</p>
    </template>
  </article>
</template>

<script setup lang="ts">
// 无项目会话的 Wiki 引导卡。三条出口（选已登记项目 / 登记现有目录 / 新建目录）
// 汇聚到同一个宿主动作：目录校验、项目登记、会话绑定、草稿重新提交与幂等记录
// 全部由 dandelion-core 完成；这里只负责展示草稿、收集用户选择、如实反馈。
import { computed, onMounted, onBeforeUnmount, ref } from "vue";
import type { WikiDraft, WikiDraftOutcome } from "../lib/types";
import { folderPick, wikiSuggestProjectName, knowledgeItemSaveDraft } from "../lib/ipc";
import { useSessionStore } from "../stores/session";

const { draft } = defineProps<{ draft: WikiDraft }>();
const store = useSessionStore();
const KIND_LABELS: Record<string, string> = { flow: "流程", decision: "决策", pitfall: "坑", term: "术语" };

const busy = ref(false);
const error = ref("");
const done = ref<WikiDraftOutcome | null>(null);
const pickedProject = ref("");
const naming = ref(false);
const locallySaved = ref(false);
const savedAlready = computed(()=>locallySaved.value || !!draft.knowledgeDocumentId || draft.mode === 'saved_knowledge');
const nameLoading=ref(false);
let disposed=false;
const newName = ref(draft.projectName || "");
const parent = ref("");

const doneName = computed(() => {
  const ws = done.value?.workspace ?? "";
  return store.projects.find((p) => p.workspace === ws)?.name || ws;
});
const targetPath = computed(() => {
  if (!parent.value || !newName.value.trim()) return "";
  const sep = parent.value.includes("\\") ? "\\" : "/";
  return parent.value.endsWith(sep) ? parent.value + newName.value.trim() : parent.value + sep + newName.value.trim();
});

async function submit(payload: Parameters<typeof store.resolveWikiDraft>[1]) {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  try {
    done.value = await store.resolveWikiDraft(draft, payload);
  } catch (err) {
    // 目录、登记、提交任一环节失败：草稿仍在，用户可换个选择重试。
    error.value = err instanceof Error ? err.message : String(err);
  } finally {
    busy.value = false;
  }
}

function chooseProject() {
  if (pickedProject.value) void submit({ mode: "project", projectId: pickedProject.value });
}

async function chooseExistingDir() {
  const path = await folderPick();
  if (path) void submit({ mode: "directory", path });
}

async function startNewProject() {
  error.value = "";
  naming.value = true;
  if(!newName.value) await generateName();
}

async function generateName(){
  if(nameLoading.value || busy.value)return;
  nameLoading.value=true;error.value='';
  try{const name=await wikiSuggestProjectName(draft.draftId);if(!disposed)newName.value=name;}
  catch(err){if(!disposed)error.value=err instanceof Error?err.message:String(err);}
  finally{if(!disposed)nameLoading.value=false;}
}
async function saveLocally(){
  if(busy.value)return;busy.value=true;error.value='';
  try{await knowledgeItemSaveDraft(draft.draftId);locallySaved.value=true;}
  catch(err){error.value=String(err);}finally{busy.value=false;}
}
onBeforeUnmount(()=>{disposed=true;});

async function pickParent() {
  const picked = await folderPick();
  if (picked) parent.value = picked;
}

function confirmNewProject() {
  if (!parent.value || !newName.value.trim()) return;
  // targetPath 已在按钮上方展示，用户确认的正是这个绝对路径。
  void submit({ mode: "new", parent: parent.value, name: newName.value.trim(), publish: true });
}

async function dismiss() {
  if (busy.value) return;
  busy.value = true;
  try {
    await store.cancelWikiDraft(draft);
  } finally {
    busy.value = false;
  }
}
</script>

<style scoped>
.wiki-draft { display: grid; gap: 8px; margin: 12px 0; padding: 14px 16px; border: 1px solid var(--border); border-left: 3px solid #d99a2b; border-radius: var(--radius-md); background: var(--bg); }
.wiki-draft header { display: flex; align-items: center; gap: 8px; }
.wiki-draft header strong { font-size: var(--fs-sm); }
.wiki-draft-kind { padding: 1px 7px; border-radius: 4px; background: color-mix(in srgb, var(--accent) 12%, var(--bg-2)); color: var(--accent); font-size: var(--fs-xs); }
.wiki-draft-write { font-size: var(--fs-xs); color: var(--muted); }
.wiki-draft-dismiss { margin-left: auto; border: 0; padding: 2px 6px; background: none; color: var(--muted); cursor: pointer; font: inherit; font-size: var(--fs-xs); }
.wiki-draft-note, .wiki-draft-path { margin: 0; color: var(--muted); font-size: var(--fs-xs); line-height: 1.5; }
.wiki-draft-content { margin: 0; padding: 8px 10px; border-left: 2px solid var(--border); background: var(--bg-2); border-radius: 4px; white-space: pre-wrap; line-height: 1.55; font-size: var(--fs-sm); }
.wiki-draft-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.wiki-draft-actions select, .wiki-draft-new input { padding: 7px 10px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-2); color: var(--text); font: inherit; font-size: var(--fs-sm); }
.wiki-draft-actions select { min-width: 180px; }
.wiki-draft-actions button, .wiki-draft-new button { padding: 7px 12px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--bg-2); color: var(--text); cursor: pointer; font: inherit; font-size: var(--fs-sm); }
.wiki-draft-actions button.primary { border-color: var(--accent); background: color-mix(in srgb, var(--accent) 16%, var(--bg-2)); color: var(--accent); }
.wiki-draft-actions button:disabled { opacity: .55; cursor: default; }
.wiki-draft summary {cursor:pointer;color:var(--muted);font-size:var(--fs-xs);padding:4px 0}.wiki-draft-new label{display:grid;gap:6px;font-size:var(--fs-sm)}
.wiki-draft-new { display: grid; gap: 8px; padding-top: 4px; border-top: 1px dashed var(--border); }
.wiki-draft-path code { user-select: all; }
.wiki-draft-error { margin: 0; color: #b3453a; font-size: var(--fs-xs); }
.wiki-draft-done { margin: 0; color: var(--muted); font-size: var(--fs-sm); }
</style>
