<template>
  <div class="tabs">
    <button
      v-for="id in store.openIds"
      :key="id"
      type="button"
      class="tab"
      :class="{ on: store.mainView === 'chat' && store.activeId === id }"
      :title="tabTitle(id)"
      @click="store.focusSession(id)"
      @auxclick.middle.prevent="store.closeTab(id)"
    >
      <span class="tab-label">{{ store.records[id]?.title || "未命名任务" }}</span>
      <!-- 空间名跟着标签走。多空间之后，两个空间里各开一条「未命名任务」是完全
           正常的，光看标题没法分辨它们在哪个目录里 —— 标签栏就成了两个同名按钮。
           名字只给登记过的空间：没登记的目录在侧栏里归在「未归属」，这里同理用
           目录名兜底会显示一个用户从没叫过的名字。 -->
      <span v-if="spaceOf(id)" class="tab-space">{{ spaceOf(id) }}</span>
      <span class="x" title="关闭 Tab（任务仍在列表）" @click.stop="store.closeTab(id)">×</span>
    </button>
    <button
      v-if="store.settingsTabOpen"
      type="button"
      class="tab"
      :class="{ on: store.mainView === 'settings' }"
      @click="store.focusSettings()"
    >
      <span class="tab-label">设置</span>
      <span class="x" title="关闭设置 Tab" @click.stop="store.settingsTabOpen = false; store.mainView = 'chat'">×</span>
    </button>
    <!-- 标签栏内的新建入口（设计稿）。侧栏的「新对话」在项目视图下会被资源树替换，
         这里保证标签栏自身随时能开新任务。 -->
    <button type="button" class="tab-new" :title="`新对话（${shortcut('Ctrl+N')}）`" aria-label="新对话" @click="store.newTask()">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3.4v9.2M3.4 8h9.2" /></svg>
    </button>
    <p v-if="!store.openIds.length && !store.settingsTabOpen" class="muted pad">从左侧打开或新建任务</p>
  </div>
</template>

<script setup lang="ts">
import { useSessionStore } from "../stores/session";
import { shortcut } from "../lib/platform";

const store = useSessionStore();

/**
 * 这条对话所属空间的**显示名**。
 *
 * 认的是 `projects` 里登记过的目录：没登记的目录在侧栏里就是「未归属」，这里如果
 * 退回目录的最后一段，会显示出一个用户从没叫过的名字（和侧栏自相矛盾）。
 */
function spaceOf(id: string): string {
  const rec = store.records[id];
  const workspace = rec?.prefs?.workspace || rec?.workspace;
  if (!workspace) return "";
  return store.projects.find((project) => project.workspace === workspace)?.name ?? "";
}

function tabTitle(id: string): string {
  const rec = store.records[id];
  const title = rec?.title || "未命名任务";
  const workspace = rec?.prefs?.workspace || rec?.workspace;
  return workspace ? `${title}\n${workspace}` : title;
}
</script>
