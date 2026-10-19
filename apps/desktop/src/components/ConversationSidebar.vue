<template>
  <aside class="sidebar conversation-sidebar" aria-label="对话列表">
    <!-- 品牌区。原本这里是实心 logo + 标语 + 空间卡片 + 实心主按钮四层堆叠，
         侧栏顶部比正文还重，紫色出现三次互相抢。现在只留 logo + 名字一行：
         品牌是身份标识，不是导航，不必挤占操作区。 -->
    <header class="side-brand">
      <span class="side-logo"><BrandMark /></span>
      <span class="side-brand-text"><strong>蒲公英</strong></span>
      <button type="button" class="side-search-toggle" :class="{ on: searchOpen }" title="搜索对话" aria-label="搜索对话" @click="toggleSearch">
        <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="6.9" cy="6.9" r="4.5" /><path d="M10.4 10.4 13.6 13.6" /></svg>
      </button>
      <button type="button" class="side-search-toggle" :class="{ on: store.selectMode }" :title="store.selectMode ? '结束多选' : '管理对话'" :aria-label="store.selectMode ? '结束多选' : '管理对话'" @click="store.toggleSelectMode()">
        <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.1" y="2.1" width="8.2" height="8.2" rx="1.35" /><rect x="5.7" y="5.7" width="8.2" height="8.2" rx="1.35" /><path d="m7.5 9.7 1.35 1.45 2.85-3" /></svg>
      </button>
    </header>
    <button type="button" class="side-new" :title="activeProjectId ? `在「${activeProjectName}」中新建对话` : `新建对话（${shortcut('Ctrl+N')}）`" @click="newConversation">
      <span class="side-new-icon">
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="M3 2.6h6.4a1.6 1.6 0 0 1 1.6 1.6v6.4a1.6 1.6 0 0 1-1.6 1.6H4.2a1.6 1.6 0 0 1-1.6-1.6V4.2A1.6 1.6 0 0 1 4.2 2.6" />
          <path d="m7.1 10.9.45-2 3.35-3.35a1.05 1.05 0 1 1 1.48 1.48L9 10.47z" />
        </svg>
      </span>
      <span>新对话</span>
    </button>
    <!-- 搜索是一个**字段**，不是一个按钮块：所以它保留边框与内凹的底色，和上面那行
         「新对话」区分开 —— 一个是筛选输入，一个是动作入口，形态不该一样。
         放大镜走内联 SVG 而不是 ⌕ 这个字形：字形靠字体兜底，字干粗细和基线都随字体
         变，和这一列其余图标（全是 16 视框的描边 SVG）不是一个语言。 -->
    <div v-if="searchOpen" class="side-search">
      <span class="side-search-icon">
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="6.9" cy="6.9" r="4.5" />
          <path d="M10.4 10.4 13.6 13.6" />
        </svg>
      </span>
      <input v-model="query" placeholder="搜索全部对话" @input="onSearch" />
      <!-- 清空只在有输入时出现。搜索是这一列里唯一会替换掉整个列表的控件，
           「怎么退出这个状态」必须看得见 —— 只能靠手动删空字符串太隐蔽。 -->
      <button
        v-if="query"
        type="button"
        class="side-search-clear"
        title="清空搜索"
        aria-label="清空搜索"
        @click="clearSearch"
      >
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.6 4.6 11.4 11.4M11.4 4.6 4.6 11.4" /></svg>
      </button>
    </div>
    <!-- 批量选择不是常驻导航。单条对话的删除/归档/复制均在右键菜单；这套低频、
         它和上面两行共用同一条 13px 内轨：原来这排按钮是 padding 0 2px，文字落在
         14px 上，而搜索框里的内容在 25px —— 整条工具条不在任何一条轨上，这是它
         看着「贴歪了」的真正原因，和按钮本身的样式无关。 -->
    <div v-if="store.selectMode" class="list-bar selection-bar">
      <span class="lb-count">已选 {{ store.selectedIds.length }}</span>
      <span class="lb-gap" />
      <button type="button" class="lb-btn icon-only" :title="allPicked ? '取消全选' : '全选当前列表'" :aria-label="allPicked ? '取消全选' : '全选当前列表'" @click="store.selectAll(visibleIds)">
        <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.2" y="2.4" width="11.6" height="11.2" rx="2" /><path d="m4.8 8 1.7 1.8L11 5.6" /></svg>
      </button>
      <button type="button" class="lb-btn icon-only danger" title="删除所选对话" aria-label="删除所选对话" :disabled="!store.selectedIds.length" @click="store.requestBulkDelete()">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 4.5h9M6 4.5V3h4v1.5M5 6.5v5M8 6.5v5M11 6.5v5M4.5 4.5l.7 9h5.6l.7-9" /></svg>
      </button>
      <button type="button" class="lb-btn icon-only" title="结束多选" aria-label="结束多选" @click="store.toggleSelectMode()">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.6 4.6 11.4 11.4M11.4 4.6 4.6 11.4" /></svg>
      </button>
    </div>
    <div v-if="query.trim()" class="hits">
      <p v-if="!store.searchHits.length" class="muted more">没有匹配的对话。</p>
      <button v-for="hit in store.searchHits" :key="hit.id + hit.snippet" type="button" class="hit" @click="store.openSession(hit.id)">
        <strong>{{ hit.title || "未命名任务" }}</strong><span>{{ hit.snippet }}</span>
      </button>
    </div>
    <div v-else class="tree conversation-tree">
      <section class="sidebar-section project-section">
        <header class="sidebar-section-head project-section-head">
          <button type="button" class="project-section-toggle" :aria-expanded="projectsExpanded" @click="projectsExpanded = !projectsExpanded">
            <span>项目</span><svg viewBox="0 0 16 16" aria-hidden="true" :class="{ open: projectsExpanded }"><path d="m5 6 3 3 3-3" /></svg>
          </button>
          <span class="project-section-actions">
            <button type="button" title="项目操作" aria-label="项目操作" @click="openProjectSectionMenu"><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="3" cy="8" r="1" /><circle cx="8" cy="8" r="1" /><circle cx="13" cy="8" r="1" /></svg></button>
            <button type="button" title="打开文件夹作为项目" aria-label="打开文件夹作为项目" @click="store.newProject()"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3v10M3 8h10" /></svg></button>
          </span>
        </header>
        <template v-if="projectsExpanded">
        <div v-for="project in store.projects" :key="project.id" class="project-group">
          <div class="project-item" :class="{ on: activeProjectId === project.id }" @contextmenu="onProjectMenu($event, project)">
            <button type="button" class="project-open" :aria-expanded="expandedProjects.has(project.id)" @click="selectProject(project.id)">
              <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 3.8h4.1l1.3 1.7h6.6v7.1H2z" /></svg>
              <span>{{ project.name }}</span>
              <svg class="project-chevron" viewBox="0 0 16 16" aria-hidden="true" :class="{ expanded: expandedProjects.has(project.id) }"><path d="m6 4 4 4-4 4" /></svg>
            </button>
            <button type="button" class="project-more" title="项目操作" aria-label="项目操作" @click.stop="onProjectMenu($event, project)">
              <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="3" cy="8" r="1" /><circle cx="8" cy="8" r="1" /><circle cx="13" cy="8" r="1" /></svg>
            </button>
            <button type="button" class="project-new-task" title="在此项目中新建对话" aria-label="在此项目中新建对话" @click.stop="newProjectConversation(project)">
              <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 2.6h7.2A1.8 1.8 0 0 1 12 4.4v7.2a1.8 1.8 0 0 1-1.8 1.8H4.4A1.8 1.8 0 0 1 2.6 11.6V4.4A1.8 1.8 0 0 1 4.4 2.6" /><path d="m8.4 10.8.55-2.15 3.1-3.1a1.05 1.05 0 0 1 1.48 1.48l-3.1 3.1z" /></svg>
            </button>
          </div>
          <template v-if="expandedProjects.has(project.id)">
          <TaskRow v-for="node in projectRoots(project.workspace)" :key="node.id" :node="node" :active="store.activeId === node.id" :open="store.openIds.includes(node.id)" :detached="store.detachedIds.includes(node.id)" :depth="0" compact @open="openTask(node.id)" @menu="onMenu" />
          <p v-if="!projectRoots(project.workspace).length" class="project-empty">暂无对话</p>
          </template>
        </div>
        <p v-if="!store.projects.length" class="sidebar-section-empty">尚未添加项目</p>
        </template>
      </section>
      <section class="sidebar-section recent-section">
        <header class="sidebar-section-head"><span>最近</span></header>
        <TaskRow v-for="node in recentRoots" :key="node.id" :node="node" :active="store.activeId === node.id" :open="store.openIds.includes(node.id)" :detached="store.detachedIds.includes(node.id)" :depth="0" compact @open="openTask(node.id)" @menu="onMenu" />
      </section>
      <p v-if="!recentRoots.length" class="muted more">
        收件箱暂无对话。
      </p>
      <p v-if="store.hasMore" class="muted more">仅显示最近 {{ store.listLimit }} 条对话。</p>
      <!-- 归档区。归档只是把对话移出主列表，不是删除，所以必须有一个能找回它们
           的入口，否则右键「归档」就成了单向操作。默认折叠，不干扰主列表。 -->
      <template v-if="store.archivedRoots.length">
        <button type="button" class="archive-toggle" :aria-expanded="archivedExpanded" @click="archivedExpanded = !archivedExpanded">
          <span class="caret" :class="{ open: archivedExpanded }"><svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.6 10.4 8 6 12.4" /></svg></span>
          已归档
          <span class="count">{{ store.archivedRoots.length }}</span>
        </button>
        <template v-if="archivedExpanded">
          <TaskRow v-for="node in store.archivedRoots" :key="node.id" :node="node" :active="store.activeId === node.id" :open="store.openIds.includes(node.id)" :detached="store.detachedIds.includes(node.id)" :depth="0" compact @open="openTask(node.id)" @menu="onMenu" />
        </template>
      </template>
    </div>
    <div v-if="menu" class="ctx conversation-ctx" :style="{ top: menu.y + 'px', left: menu.x + 'px' }" @click.stop>
      <button type="button" @click="doOpen"><span>↵</span>打开</button>
      <button type="button" :disabled="store.detachedIds.includes(menu.node.id)" @click="doDetach"><span>◫</span>分离为独立窗口</button>
      <button type="button" @click="doPin"><span>⚐</span>{{ menu.node.pinned ? "取消置顶" : "置顶" }}</button>
      <button type="button" :disabled="menu.node.kind !== 'user'" @click="doCopy"><span>⧉</span>复制对话</button>
      <div class="ctx-rule" />
      <button type="button" @click="doArchive"><span>▱</span>{{ menu.node.archived ? "取消归档" : "归档" }}</button>
      <button type="button" class="danger" @click="doDelete"><span>✕</span>删除</button>
    </div>
    <div v-if="projectMenu" class="ctx project-menu" :style="{ top: projectMenu.y + 'px', left: projectMenu.x + 'px' }" @click.stop>
      <button type="button" @click="doAddProject"><span>＋</span>打开文件夹作为项目…</button>
      <button type="button" @click="doReveal"><span>↗</span>打开文件夹</button>
      <div class="ctx-rule" />
      <button type="button" class="danger" @click="doDeleteProject"><span>×</span>删除项目与对话…</button>
    </div>
    <div v-if="projectSectionMenu" class="ctx project-menu" :style="{ top: projectSectionMenu.y + 'px', left: projectSectionMenu.x + 'px' }" @click.stop>
      <button type="button" @click="doAddProject"><span>＋</span>打开文件夹作为项目…</button>
      <button type="button" @click="doPruneEmptyProjects"><span>⌁</span>清理空项目</button>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { useSessionStore } from "../stores/session";
import { shortcut } from "../lib/platform";
import type { ProjectRow, TaskNode } from "../lib/types";
import BrandMark from "./BrandMark.vue";
import TaskRow from "./TaskRow.vue";

const store = useSessionStore();
const query = ref("");
const searchOpen = ref(false);
const menu = ref<{ x: number; y: number; node: TaskNode } | null>(null);
const projectMenu = ref<{ x: number; y: number; project: ProjectRow } | null>(null);
const projectSectionMenu = ref<{ x: number; y: number } | null>(null);
const archivedExpanded = ref(false);
const projectsExpanded = ref(true);
const expandedProjects = ref(new Set<string>());
const activeProjectId = ref<string | null>(null);
const activeProjectName = computed(() => store.projects.find((project) => project.id === activeProjectId.value)?.name ?? "最近");

watch(() => store.treeFocusSeq, async (seq) => {
  if (!seq) return;
  searchOpen.value = false;
  clearSearch();
  projectsExpanded.value = true;
  const containsActive = (node: TaskNode): boolean => node.id === store.activeId || node.children.some(containsActive);
  for (const project of store.projects) {
    if (projectRoots(project.workspace).some(containsActive)) expandedProjects.value.add(project.id);
  }
  if (store.archivedRoots.some(containsActive)) archivedExpanded.value = true;
  await nextTick();
  const activeRow = document.querySelector<HTMLElement>(".conversation-tree .task-row.on");
  activeRow?.focus({ preventScroll: true });
  activeRow?.scrollIntoView({ block: "nearest" });
}, { immediate: true });

function workspaceFor(node: TaskNode): string {
  return store.rows.find((row) => row.id === node.id)?.workspace ?? "";
}
function selectProject(id: string) {
  if (expandedProjects.value.has(id)) {
    expandedProjects.value.delete(id);
    if (activeProjectId.value === id) activeProjectId.value = null;
  } else {
    expandedProjects.value.add(id);
    activeProjectId.value = id;
  }
}
function newConversation() {
  const project = store.projects.find((item) => item.id === activeProjectId.value);
  activeProjectId.value = null;
  if (project) void store.newTaskIn(project.workspace);
  else void store.newTask();
}
function openTask(id: string) {
  // Codex 的侧栏只强调一个焦点：打开任务后，项目不再同时维持“被选中”的灰底。
  activeProjectId.value = null;
  void store.openSession(id);
}
function newProjectConversation(project: ProjectRow) {
  expandedProjects.value.add(project.id);
  projectsExpanded.value = true;
  activeProjectId.value = null;
  void store.newTaskIn(project.workspace);
}

// 全选的作用范围＝此刻真正列出来的那些行（含展开的归档项与子节点），不是全库 ——
// 折叠起来看不见的东西不该被一次点击牵连。
function flatten(nodes: TaskNode[]): string[] {
  return nodes.flatMap((n) => [n.id, ...flatten(n.children)]);
}
const recentRoots = computed(() => {
  return store.tree.filter((node) => !store.projects.some((project) => project.workspace === workspaceFor(node)));
});
function projectRoots(workspace: string) { return store.tree.filter((node) => workspaceFor(node) === workspace); }
const visibleIds = computed(() => {
  const ids = [...(projectsExpanded.value ? store.projects.filter((project) => expandedProjects.value.has(project.id)).flatMap((project) => flatten(projectRoots(project.workspace))) : []), ...flatten(recentRoots.value)];
  return archivedExpanded.value ? [...ids, ...flatten(store.archivedRoots)] : ids;
});
const allPicked = computed(
  () => visibleIds.value.length > 0 && visibleIds.value.every((id) => store.selectedIds.includes(id)),
);

function toggleSearch() {
  searchOpen.value = !searchOpen.value;
  if (!searchOpen.value) clearSearch();
}

let timer: number | undefined;
function onSearch() {
  window.clearTimeout(timer);
  timer = window.setTimeout(() => void store.search(query.value), 200);
}
function clearSearch() {
  query.value = "";
  window.clearTimeout(timer);
  void store.search("");
}

// ---- 右键菜单 --------------------------------------------------------------
function closeMenus() {
  menu.value = null;
  projectMenu.value = null;
  projectSectionMenu.value = null;
}
function armDismiss() {
  window.setTimeout(() => document.addEventListener("click", onDocClick, { once: true }), 0);
}

function onMenu(payload: { ev: MouseEvent; node: TaskNode }) {
  payload.ev.preventDefault();
  payload.ev.stopPropagation();
  closeMenus();
  menu.value = { x: payload.ev.clientX, y: payload.ev.clientY, node: payload.node };
  armDismiss();
}

function onProjectMenu(ev: MouseEvent, project: ProjectRow) {
  ev.preventDefault();
  ev.stopPropagation();
  closeMenus();
  projectMenu.value = { x: ev.clientX, y: ev.clientY, project };
  armDismiss();
}
function openProjectSectionMenu(ev: MouseEvent) {
  ev.preventDefault();
  ev.stopPropagation();
  closeMenus();
  projectSectionMenu.value = { x: ev.clientX, y: ev.clientY };
  armDismiss();
}
function doReveal() {
  const project = projectMenu.value?.project;
  closeMenus();
  if (project?.workspace) void store.revealProject(project.workspace);
}
function doAddProject() {
  closeMenus();
  void store.newProject();
}
function doPruneEmptyProjects() {
  closeMenus();
  void store.previewEmptyProjects();
}
function doDeleteProject() {
  const project = projectMenu.value?.project;
  closeMenus();
  if (project?.id) store.requestDeleteProject(project);
}

function doOpen() { if (!menu.value) return; void store.openSession(menu.value.node.id); menu.value = null; }
function doDetach() { if (!menu.value) return; const id = menu.value.node.id; menu.value = null; void store.detachSession(id); }
function doCopy() { if (!menu.value) return; void store.duplicateTask(menu.value.node.id); menu.value = null; }
function doPin() { if (!menu.value) return; const { id, pinned } = menu.value.node; menu.value = null; void store.setPinned(id, !pinned); }
function doArchive() { if (!menu.value) return; const { id, archived } = menu.value.node; menu.value = null; void store.setArchived(id, !archived); }
// 删除走 store 的 deleteTarget 确认弹窗，不直接调 ipc —— 后端 session_delete 是
// 级联的（连子任务和它们的消息一起删），必须让用户先看到影响范围。
function doDelete() { if (!menu.value) return; store.deleteTarget = menu.value.node; menu.value = null; }
function onDocClick() { closeMenus(); }
</script>
