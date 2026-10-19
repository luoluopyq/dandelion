<template>
  <aside class="sidebar">
    <header class="side-head">
      <span>项目管理</span>
      <button type="button" class="new" title="选择目录并新建项目" @click="store.newProject()"><span>＋</span>新建项目</button>
    </header>
    <input
      v-model="query"
      class="search"
      placeholder="搜索项目"
    />
    <div class="tree">
      <section v-for="project in projects" :key="project.id" class="project-group">
        <div class="project-row" @contextmenu="onProjectMenu($event, project)">
          <button type="button" class="project-toggle" :class="{ active: store.projectExplorer?.id === project.id }" @click="store.openProjectExplorer(project)"><b>▣ {{ project.name }}</b><small>{{ project.workspace }}</small></button>
        </div>
      </section>
      <p v-if="!projects.length" class="muted more">还没有项目。选择一个本地文件夹开始。</p>
    </div>

    <div v-if="gitMenu" class="ctx project-git-ctx" :style="{ top: gitMenu.y + 'px', left: gitMenu.x + 'px' }" @click.stop>
      <strong>{{ gitMenu.project.name }}</strong>
      <p v-if="gitMenu.loading">正在读取 Git 状态…</p>
      <template v-else-if="gitMenu.snapshot?.repository">
        <button type="button" class="ctx-branch" @click="branchMenuOpen = !branchMenuOpen"><span>⑂</span><b>{{ gitMenu.snapshot.branch || '选择分支' }}</b><i>›</i></button>
        <div v-if="branchMenuOpen" class="ctx-branch-list"><button v-for="branch in gitMenu.snapshot.localBranches" :key="branch.name" type="button" :class="{ on: branch.current }" @click="switchProjectBranch(branch.name)"><span>{{ branch.current ? '✓' : '' }}</span>{{ branch.name }}</button></div>
        <div class="ctx-rule" />
        <button type="button" @click="openProjectReview('diff')"><span>◫</span>查看本地变更</button>
        <button type="button" @click="openProjectReview('history')"><span>◷</span>查看提交历史</button>
        <div class="ctx-rule" />
        <button type="button" :disabled="gitMenu.busy" @click="projectNetwork('fetch')"><span>↻</span>获取远程更新</button>
        <button type="button" :disabled="gitMenu.busy" @click="projectNetwork('pull')"><span>↓</span>拉取当前分支</button>
        <button type="button" :disabled="gitMenu.busy" @click="projectNetwork('push')"><span>↑</span>推送当前分支</button>
      </template>
      <template v-else><p>{{ gitMenu.snapshot?.gitAvailable ? '该项目尚未初始化 Git。' : '未检测到 Git。' }}</p></template>
      <div class="ctx-rule" />
      <button v-if="gitMenu.project.managed" type="button" class="project-archive" @click="deleteProject"><span>×</span>删除项目</button>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { useSessionStore } from "../stores/session";
import { gitFetch, gitPull, gitPush, gitSnapshot, gitSwitchBranch } from "../lib/ipc";
import type { GitSnapshot, ProjectRow } from "../lib/types";

const store = useSessionStore();
const query = ref("");
const gitMenu = ref<{ x: number; y: number; project: ProjectRow & { managed: boolean }; snapshot: GitSnapshot | null; loading: boolean; busy: boolean } | null>(null);
const branchMenuOpen = ref(false);
const projects = computed(() => store.projects.filter((project) => `${project.name} ${project.workspace}`.toLowerCase().includes(query.value.trim().toLowerCase())).map((project) => ({ ...project, managed: true })));

async function onProjectMenu(ev: MouseEvent, project: ProjectRow & { managed: boolean }) {
  ev.preventDefault(); ev.stopPropagation();
  branchMenuOpen.value = false;
  gitMenu.value = { x: ev.clientX, y: ev.clientY, project, snapshot: null, loading: true, busy: false };
  try { gitMenu.value.snapshot = await gitSnapshot(project.workspace); } finally { if (gitMenu.value) gitMenu.value.loading = false; }
  window.setTimeout(() => document.addEventListener("click", onDocClick, { once: true }), 0);
}
function openProjectReview(view: "diff" | "history") { if (!gitMenu.value) return; const root = gitMenu.value.project.workspace; gitMenu.value = null; store.openGitReview({ root, view }); }
async function switchProjectBranch(branch: string) { if (!gitMenu.value || !branch) return; try { gitMenu.value.snapshot = await gitSwitchBranch(gitMenu.value.project.workspace, branch); branchMenuOpen.value = false; } catch (err) { store.panelError = String(err); } }
async function projectNetwork(action: "fetch" | "pull" | "push") { if (!gitMenu.value) return; gitMenu.value.busy = true; try { gitMenu.value.snapshot = await ({ fetch: gitFetch, pull: gitPull, push: gitPush }[action])(gitMenu.value.project.workspace); } catch (err) { store.panelError = String(err); } finally { if (gitMenu.value) gitMenu.value.busy = false; } }
function deleteProject() { if (!gitMenu.value) return; const project = gitMenu.value.project; if (!window.confirm(`删除“${project.name}”项目管理记录？项目目录和其中的文件不会被删除。`)) return; gitMenu.value = null; void store.deleteProject(project.id); }

function onDocClick() {
  gitMenu.value = null;
  branchMenuOpen.value = false;
}
</script>
