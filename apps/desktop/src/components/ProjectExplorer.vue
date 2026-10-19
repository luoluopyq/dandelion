<template>
  <section class="project-explorer-page">
    <div class="project-explorer-layout">
      <aside class="project-browser">
        <header class="project-explorer-header">
          <button type="button" class="project-explorer-title" :title="root" @click="projectsOpen = !projectsOpen" @contextmenu.prevent.stop="openProjectGitMenu"><small>资源管理器</small><strong>{{ project?.name || "项目" }} <i>⌄</i></strong></button>
          <div class="project-explorer-actions"><button type="button" title="Git 管理" aria-label="Git 管理" @click="openGit"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 1.75v12.5M4 4.5h4.2a2.1 2.1 0 1 0 0-1.5M4 10.5h4.2a2.1 2.1 0 1 1 0 1.5M4 8V6" /><circle cx="10.5" cy="2.5" r="1.75" /><circle cx="10.5" cy="13.5" r="1.75" /></svg></button><button type="button" title="新建文件" aria-label="新建文件" @click="createEntry(false)"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 1.75h6l3 3V14H3zM9 1.75v3h3M8 7v5M5.5 9.5h5" /></svg></button><button type="button" title="新建文件夹" aria-label="新建文件夹" @click="createEntry(true)"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M1.75 4.5h4l1.3 1.5h7.2v7.75H1.75zM8 8v4M6 10h4" /></svg></button><button type="button" title="刷新目录" aria-label="刷新目录" @click="loadFolder(currentFolder)"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 6.5V3.25m0 0H9.75M13 3.25A5.5 5.5 0 1 0 13.4 10" /></svg></button></div>
          <div v-if="projectsOpen" class="project-switcher"><button v-for="item in store.projects" :key="item.id" type="button" :class="{ on: item.id === project?.id }" @click="selectProject(item)"><span>▣</span>{{ item.name }}</button><div class="ctx-rule" /><button type="button" @click="addProject"><span>＋</span>添加项目</button></div>
        </header>
        <p v-if="error" class="project-explorer-error">{{ error }}</p>
        <nav v-if="currentFolder" class="project-crumbs"><button type="button" @click="loadFolder('')">{{ project?.name || "根目录" }}</button><template v-for="crumb in crumbs" :key="crumb.path"><span>›</span><button type="button" @click="loadFolder(crumb.path)">{{ crumb.name }}</button></template></nav>
        <div class="project-entry-list">
          <button v-if="currentFolder" type="button" class="project-up" @click="goUp">‹ 上一级</button>
          <button v-for="entry in entries" :key="entry.relativePath" type="button" :class="{ selected: selected?.relativePath === entry.relativePath }" @click="openEntry(entry)" @contextmenu.prevent="entryMenu(entry, $event)"><span class="project-entry-icon">{{ entry.isDir ? '▸' : '▤' }}</span><span :title="entry.name">{{ entry.name }}</span><small v-if="!entry.isDir">{{ formatBytes(entry.size) }}</small></button>
          <p v-if="!entries.length" class="muted">此目录为空。</p>
        </div>
      </aside>
      <main class="project-file-view">
        <template v-if="selected">
          <header class="project-file-tab"><span>▤</span><strong>{{ selected.name }}</strong><i v-if="dirty" title="有未保存的修改" /></header>
          <p v-if="preview?.binary" class="project-file-notice">这是二进制文件，不能在此编辑。</p>
          <template v-else>
            <textarea v-model="draft" class="project-file-editor" spellcheck="false" @input="dirty = true" />
            <p v-if="preview?.truncated" class="project-file-limit">文件超过 256 KB，当前仅加载前 256 KB；为避免意外覆盖，不能保存。</p>
          </template>
        </template>
        <div v-else class="project-file-empty"><strong>选择一个文件</strong><p>文件内容会在这里打开，不会新建对话任务。</p></div>
      </main>
    </div>
    <div v-if="menu" class="ctx project-file-ctx" :style="{ top: menu.y + 'px', left: menu.x + 'px' }" @click.stop>
      <button type="button" @click="renameEntry(menu.entry)">重命名</button>
      <button type="button" class="danger" @click="deleteEntry(menu.entry)">删除</button>
      <div class="ctx-rule" />
      <p v-if="menu.gitLoading" class="project-file-ctx-status">正在读取 Git 状态…</p>
      <template v-else-if="menu.git?.repository">
        <button type="button" @click="reviewEntry">Git：查看差异</button>
        <button v-if="gitEntry" type="button" @click="toggleStage">Git：{{ gitEntry.staged ? '取消暂存' : '暂存' }}</button>
        <button v-if="gitEntry && gitEntry.status !== '未跟踪'" type="button" @click="restoreEntry">Git：还原改动</button>
        <button type="button" @click="reviewHistory">Git：查看提交历史</button>
        <button type="button" @click="openGit">Git：打开管理器</button>
      </template>
      <p v-else-if="menu.git" class="project-file-ctx-status">此项目尚未初始化 Git。</p>
    </div>
    <div v-if="projectGitMenu" class="ctx project-file-ctx project-root-git-ctx" :style="{ top: projectGitMenu.y + 'px', left: projectGitMenu.x + 'px' }" @click.stop>
      <strong>{{ project?.name || "项目" }}</strong>
      <p v-if="projectGitMenu.loading" class="project-file-ctx-status">正在读取 Git 状态…</p>
      <template v-else-if="projectGitMenu.snapshot?.repository">
        <button type="button" class="ctx-branch" @click="projectGitMenu.branchesOpen = !projectGitMenu.branchesOpen"><span>⑂</span><b>{{ projectGitMenu.snapshot.branch || "选择分支" }}</b><i>›</i></button>
        <div v-if="projectGitMenu.branchesOpen" class="ctx-branch-list"><button v-for="branch in projectGitMenu.snapshot.localBranches" :key="branch.name" type="button" :class="{ on: branch.current }" @click="switchProjectBranch(branch.name)"><span>{{ branch.current ? "✓" : "" }}</span>{{ branch.name }}</button></div>
        <div class="ctx-rule" />
        <button type="button" @click="openProjectGitReview('diff')">Git：查看本地变更</button>
        <button type="button" @click="openProjectGitReview('commit')">Git：提交…</button>
        <button type="button" @click="openProjectGitReview('history')">Git：查看提交历史</button>
        <div class="ctx-rule" />
        <button type="button" :disabled="projectGitMenu.busy" @click="projectNetwork('fetch')">Git：获取远程更新</button>
        <button type="button" :disabled="projectGitMenu.busy" @click="projectNetwork('pull')">Git：拉取当前分支</button>
        <button type="button" :disabled="projectGitMenu.busy" @click="projectNetwork('push')">Git：推送当前分支</button>
      </template>
      <p v-else-if="projectGitMenu.snapshot" class="project-file-ctx-status">{{ projectGitMenu.snapshot.gitAvailable ? "此项目尚未初始化 Git。" : "未检测到 Git。" }}</p>
    </div>
    <div v-if="entryDialog" class="project-entry-modal-backdrop" @mousedown.self="entryDialog = null"><form class="project-entry-modal" @submit.prevent="submitEntryDialog"><header><strong>{{ entryDialog.mode === 'delete' ? '删除' : entryDialog.mode === 'rename' ? '重命名' : entryDialog.isDir ? '新建文件夹' : '新建文件' }}</strong><button type="button" aria-label="关闭" @click="entryDialog = null">×</button></header><p v-if="entryDialog.mode === 'delete'" class="project-entry-delete-copy">删除“{{ entryDialog.entry?.relativePath }}”？<br><span v-if="entryDialog.isDir">其中所有内容也会被删除。</span></p><label v-else>{{ entryDialog.mode === 'rename' ? '新名称' : '名称' }}<input ref="entryInput" v-model="entryDialog.value" autocomplete="off" spellcheck="false" /></label><p v-if="entryDialog.error">{{ entryDialog.error }}</p><footer><button type="button" @click="entryDialog = null">取消</button><button type="submit" class="button-primary" :class="{ danger: entryDialog.mode === 'delete' }" :disabled="entryDialog.busy">{{ entryDialog.busy ? '处理中…' : entryDialog.mode === 'delete' ? '删除' : '确认' }}</button></footer></form></div>
  </section>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { folderList, folderRead, gitFetch, gitPull, gitPush, gitRestoreFile, gitSnapshot, gitStage, gitSwitchBranch, workspaceCreateEntry, workspaceDeleteEntry, workspaceRenameEntry, workspaceWriteFile } from "../lib/ipc";
import type { FilePreview, FolderEntry, GitSnapshot } from "../lib/types";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const project = computed(() => store.projectExplorer);
const root = computed(() => project.value?.workspace ?? "");
const currentFolder = ref("");
const entries = ref<FolderEntry[]>([]);
const selected = ref<FolderEntry | null>(null);
const preview = ref<FilePreview | null>(null);
const draft = ref("");
const dirty = ref(false);
const saving = ref(false);
const error = ref("");
const menu = ref<{ entry: FolderEntry; x: number; y: number; git: GitSnapshot | null; gitLoading: boolean } | null>(null);
const projectGitMenu = ref<{ x: number; y: number; snapshot: GitSnapshot | null; loading: boolean; busy: boolean; branchesOpen: boolean } | null>(null);
const projectsOpen = ref(false);
const entryInput = ref<HTMLInputElement | null>(null);
const entryDialog = ref<{ mode: "create" | "rename" | "delete"; isDir: boolean; entry?: FolderEntry; value: string; busy: boolean; error: string } | null>(null);
const crumbs = computed(() => { let path = ""; return currentFolder.value.split("/").filter(Boolean).map((name) => ({ name, path: path = path ? `${path}/${name}` : name })); });
const gitEntry = computed(() => {
  const current = menu.value;
  if (!current?.git) return null;
  const path = current.entry.relativePath;
  const staged = current.git.staged.find((file) => file.path === path);
  if (staged) return { ...staged, staged: true };
  const unstaged = current.git.unstaged.find((file) => file.path === path) ?? current.git.untracked.find((file) => file.path === path);
  return unstaged ? { ...unstaged, staged: false } : null;
});

async function loadFolder(relativePath = "") {
  if (!root.value) return;
  error.value = ""; selected.value = null; preview.value = null; draft.value = ""; dirty.value = false;
  try { entries.value = await folderList(root.value, relativePath); currentFolder.value = relativePath; }
  catch (err) { error.value = String(err); entries.value = []; }
}
function goUp() { const bits = currentFolder.value.split("/").filter(Boolean); bits.pop(); void loadFolder(bits.join("/")); }
async function openEntry(entry: FolderEntry) {
  if (entry.isDir) { await loadFolder(entry.relativePath); return; }
  error.value = ""; selected.value = entry; preview.value = null; dirty.value = false;
  try { const next = await folderRead(root.value, entry.relativePath); preview.value = next; draft.value = next.content; }
  catch (err) { error.value = `无法读取文件：${String(err)}`; }
}
async function save() {
  if (!selected.value || !dirty.value || preview.value?.truncated) return;
  saving.value = true; error.value = "";
  try { preview.value = await workspaceWriteFile(root.value, selected.value.relativePath, draft.value); draft.value = preview.value.content; dirty.value = false; }
  catch (err) { error.value = `保存失败：${String(err)}`; }
  finally { saving.value = false; }
}
async function createEntry(isDir: boolean) {
  entryDialog.value = { mode: "create", isDir, value: "", busy: false, error: "" };
  await nextTick(); entryInput.value?.focus();
}
async function entryMenu(entry: FolderEntry, event: MouseEvent) {
  menu.value = { entry, x: event.clientX, y: event.clientY, git: null, gitLoading: true };
  window.setTimeout(() => document.addEventListener("click", () => menu.value = null, { once: true }), 0);
  try {
    const snapshot = await gitSnapshot(root.value);
    if (menu.value?.entry.relativePath === entry.relativePath) menu.value.git = snapshot;
  } catch (err) {
    if (menu.value?.entry.relativePath === entry.relativePath) error.value = `无法读取 Git 状态：${String(err)}`;
  } finally {
    if (menu.value?.entry.relativePath === entry.relativePath) menu.value.gitLoading = false;
  }
}
async function openProjectGitMenu(event: MouseEvent) {
  if (!root.value) return;
  menu.value = null;
  projectGitMenu.value = { x: event.clientX, y: event.clientY, snapshot: null, loading: true, busy: false, branchesOpen: false };
  window.setTimeout(() => document.addEventListener("click", () => projectGitMenu.value = null, { once: true }), 0);
  try { projectGitMenu.value.snapshot = await gitSnapshot(root.value); }
  catch (err) { error.value = `无法读取 Git 状态：${String(err)}`; }
  finally { if (projectGitMenu.value) projectGitMenu.value.loading = false; }
}
async function renameEntry(entry: FolderEntry) {
  menu.value = null; entryDialog.value = { mode: "rename", isDir: entry.isDir, entry, value: entry.name, busy: false, error: "" };
  await nextTick(); entryInput.value?.focus(); entryInput.value?.select();
}
async function submitEntryDialog() {
  const dialog = entryDialog.value; if (!dialog) return;
  const name = dialog.value.trim(); if (dialog.mode !== "delete" && !name) return;
  dialog.busy = true; dialog.error = "";
  try {
    if (dialog.mode === "delete" && dialog.entry) { await workspaceDeleteEntry(root.value, dialog.entry.relativePath); }
    else if (dialog.mode === "create") { const path = currentFolder.value ? `${currentFolder.value}/${name}` : name; await workspaceCreateEntry(root.value, path, dialog.isDir); }
    else if (dialog.entry && name !== dialog.entry.name) { const parent = dialog.entry.relativePath.split("/").slice(0, -1).join("/"); await workspaceRenameEntry(root.value, dialog.entry.relativePath, parent ? `${parent}/${name}` : name); }
    entryDialog.value = null; await loadFolder(currentFolder.value);
  } catch (err) { dialog.busy = false; dialog.error = String(err); }
}
async function deleteEntry(entry: FolderEntry) {
  menu.value = null; entryDialog.value = { mode: "delete", isDir: entry.isDir, entry, value: "", busy: false, error: "" };
}
function formatBytes(size?: number | null) { if (size == null) return ""; return size < 1024 ? `${size} B` : `${Math.ceil(size / 1024)} KB`; }
function openGit() { if (root.value) store.openGitReview({ root: root.value, view: "diff" }); }
function openProjectGitReview(view: "diff" | "commit" | "history") { if (!root.value) return; projectGitMenu.value = null; store.openGitReview({ root: root.value, view }); }
async function switchProjectBranch(branch: string) {
  if (!projectGitMenu.value || !root.value) return;
  try { projectGitMenu.value.snapshot = await gitSwitchBranch(root.value, branch); projectGitMenu.value.branchesOpen = false; window.dispatchEvent(new Event("dandelion:git-refresh")); }
  catch (err) { error.value = `Git 切换分支失败：${String(err)}`; }
}
async function projectNetwork(action: "fetch" | "pull" | "push") {
  if (!projectGitMenu.value || !root.value) return;
  projectGitMenu.value.busy = true;
  try { projectGitMenu.value.snapshot = await ({ fetch: gitFetch, pull: gitPull, push: gitPush }[action])(root.value); window.dispatchEvent(new Event("dandelion:git-refresh")); }
  catch (err) { error.value = `Git ${action} 失败：${String(err)}`; }
  finally { if (projectGitMenu.value) projectGitMenu.value.busy = false; }
}
function reviewEntry() { if (!menu.value || !root.value) return; const path = gitEntry.value?.path; menu.value = null; store.openGitReview({ root: root.value, view: "diff", path }); }
function reviewHistory() { if (!root.value) return; menu.value = null; store.openGitReview({ root: root.value, view: "history" }); }
async function toggleStage() {
  if (!menu.value || !root.value || !gitEntry.value) return;
  const entry = gitEntry.value;
  try { const snapshot = await gitStage(root.value, entry.path, entry.staged); if (menu.value) menu.value.git = snapshot; window.dispatchEvent(new Event("dandelion:git-refresh")); }
  catch (err) { error.value = `Git 暂存失败：${String(err)}`; }
}
async function restoreEntry() {
  if (!menu.value || !root.value || !gitEntry.value) return;
  const entry = gitEntry.value;
  if (!window.confirm(`还原 ${entry.path} 的未提交改动？`)) return;
  try { await gitRestoreFile(root.value, entry.path, entry.staged); menu.value = null; await loadFolder(currentFolder.value); window.dispatchEvent(new Event("dandelion:git-refresh")); }
  catch (err) { error.value = `Git 还原失败：${String(err)}`; }
}
function selectProject(item: NonNullable<typeof store.projectExplorer>) { projectsOpen.value = false; store.openProjectExplorer(item); }
// 加项目是从资源管理器里发起的，选完就该落在资源管理器里 —— 不能走 store.newProject()，
// 那条路现在会把用户带进「空间」语境（左栏列表），而不是这个面板。
function addProject() { projectsOpen.value = false; void store.pickWorkbenchFolder(); }
watch(project, () => void loadFolder(""), { deep: true });
function onKeydown(event: KeyboardEvent) {
  if (event.key.toLowerCase() !== "s" || !(event.ctrlKey || event.metaKey) || !selected.value || preview.value?.binary || preview.value?.truncated) return;
  event.preventDefault();
  void save();
}
onMounted(() => { window.addEventListener("keydown", onKeydown); void loadFolder(""); });
onUnmounted(() => { window.removeEventListener("keydown", onKeydown); menu.value = null; });
</script>
