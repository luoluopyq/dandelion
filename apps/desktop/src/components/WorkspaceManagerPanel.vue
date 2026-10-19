<template>
  <section class="spaces-page">
    <header class="spaces-header">
      <div>
        <p class="eyebrow">归属</p>
        <h1>空间管理</h1>
        <p class="spaces-subtitle">
          每个空间就是一个本地目录，空间名取自目录名，里面的对话都归这个空间所有。
          目录被外部移走或删除时，空间与它名下的对话会一起清掉。
        </p>
      </div>
      <div class="spaces-header-actions">
        <button type="button" class="button-quiet" @click="store.refreshProjects()">刷新</button>
        <button type="button" class="button-quiet" @click="store.previewEmptyProjects()">清理空空间</button>
        <button type="button" class="button-primary" @click="store.newProject()">＋ 打开文件夹</button>
      </div>
    </header>

    <p v-if="store.notice" class="spaces-notice">
      <span>{{ store.notice }}</span>
      <button type="button" aria-label="关闭提示" @click="store.notice = null">×</button>
    </p>

    <div v-if="store.spaceGroups.length" class="space-cards">
      <article
        v-for="group in store.spaceGroups"
        :key="group.workspace || UNASSIGNED"
        class="space-card"
        :class="{ on: isActive(group), dim: group.unassigned }"
      >
        <header class="space-card-head">
          <!-- 图标一律内联 SVG：`◇` / `▣` 这类字形靠字体兜底，跨平台形状不一致，
               而且和侧栏分组头里那套（文件夹 / 房屋）对不上。 -->
          <span class="space-card-icon">
            <svg v-if="!group.unassigned" viewBox="0 0 16 16" aria-hidden="true">
              <path d="M2 3.8h4.1l1.3 1.7h6.6v7.1H2z" />
            </svg>
            <svg v-else viewBox="0 0 16 16" aria-hidden="true">
              <path d="M2.6 7.1 8 3.2l5.4 3.9v4.6a1 1 0 0 1-1 1H3.6a1 1 0 0 1-1-1z" />
            </svg>
          </span>
          <div class="space-card-title">
            <strong :title="group.name">{{ group.name }}</strong>
            <small :title="groupSubtitle(group)">{{ groupSubtitle(group) }}</small>
          </div>
          <!-- 「当前」这个词在这里必须说清是谁的当前：空间不再是全局状态，
               它只是「此刻打开的那条对话落在这里」。 -->
          <span v-if="isActive(group)" class="space-card-badge">当前对话在此</span>
        </header>
        <dl class="space-card-meta">
          <div><dt>对话</dt><dd>{{ group.total }}</dd></div>
          <div><dt>最近活动</dt><dd>{{ recentLabel(group) }}</dd></div>
        </dl>
        <p v-if="!group.unassigned && !group.total" class="space-card-hint">
          还没有对话。这样的空间可以用右上角「清理空空间」一次收掉。
        </p>
        <footer class="space-card-actions">
          <button type="button" class="button-primary" :disabled="!group.sessions.length" @click="openLatest(group)">
            打开最近对话
          </button>
          <button v-if="!group.unassigned" type="button" class="button-quiet" @click="openExplorer(group)">资源管理器</button>
          <button v-if="group.id" type="button" class="button-danger" @click="askDelete(group)">删除</button>
        </footer>
      </article>
    </div>

    <p v-else class="spaces-empty">
      还没有工作空间。点右上角「打开文件夹」，把本地目录变成一个空间。
    </p>
  </section>
</template>

<script setup lang="ts">
import { useSessionStore, workspaceLabel, type SpaceGroup } from "../stores/session";
import { formatRelative } from "../lib/task-tree";

const store = useSessionStore();

const UNASSIGNED = "__unassigned__";

function isActive(group: SpaceGroup): boolean {
  return group.unassigned ? store.activeWorkspace === null : store.activeWorkspace === group.workspace;
}

// 未归属那一组的 workspace 是空的，直接把空路径显示出来等于没说。它下面的对话
// 可能来自好几个没登记成空间的目录，列出目录名才解释得清「这些对话为什么在这」。
function groupSubtitle(group: SpaceGroup): string {
  if (!group.unassigned) return group.workspace || "没有工作目录";
  const dirs = [...new Set(group.sessions.map((node) => node.workspace).filter(Boolean))];
  if (!dirs.length) return "没有工作目录的对话";
  const names = dirs.slice(0, 2).map((dir) => workspaceLabel(dir)).join("、");
  return dirs.length > 2 ? `未登记成空间的目录：${names} 等 ${dirs.length} 个` : `未登记成空间的目录：${names}`;
}

function recentLabel(group: SpaceGroup): string {
  const latest = group.sessions
    .map((node) => Date.parse(node.updatedAt) || 0)
    .reduce((max, value) => Math.max(max, value), 0);
  if (!latest) return "还没有对话";
  return formatRelative(new Date(latest).toISOString());
}

/**
 * 打开这个空间最近的一条对话。
 *
 * 没有「进入空间」这个动作了：空间不是一个可以被选中的全局状态（那正是「整个软件
 * 只有一个工作目录」的来源），只是一组对话的归属。所以这页能做的就是替你把那条
 * 最近的对话打开 —— 剩下的对话在侧栏里按空间分组列着，本来就都看得见。
 */
function openLatest(group: SpaceGroup) {
  const latest = group.sessions[0];
  if (latest) void store.openSession(latest.id);
}

function openExplorer(group: SpaceGroup) {
  store.openProjectExplorer({ id: group.id ?? "", name: group.name, workspace: group.workspace });
}

function askDelete(group: SpaceGroup) {
  if (!group.id) return;
  store.requestDeleteProject({ id: group.id, name: group.name, workspace: group.workspace });
}
</script>
