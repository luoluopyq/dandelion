<template>
  <aside v-if="visible" class="codex-workspace" :style="paneStyle" aria-label="工作区">
    <!-- 和工作台左侧那条分界线共用一份实现：拖拽、双击复位、方向键微调都在
         PaneResizer 里，这里只声明「面板贴右边，指针往左拖才是变宽」。 -->
    <PaneResizer
      class="workbench-resizer"
      :model-value="session.workbenchWidth"
      :min="WORKBENCH_MIN_WIDTH"
      :max="WORKBENCH_MAX_WIDTH"
      :reset="WORKBENCH_DEFAULT_WIDTH"
      label="调整工作区宽度"
      inverted
      @update:model-value="session.setWorkbenchWidth($event, false)"
      @commit="session.setWorkbenchWidth($event)"
    />

    <header class="workspace-head">
      <button v-if="tab" type="button" class="workspace-head-tool" title="返回工具列表" @click="backToLauncher">
        <span class="workspace-head-caret">‹</span>
        <span class="workspace-icon">{{ currentTool.icon }}</span>
        <strong>{{ currentTool.label }}</strong>
      </button>
      <span v-else class="workspace-head-tool"><span class="workspace-icon">▦</span><strong>工作区</strong></span>
      <button
        type="button"
        class="workspace-head-root"
        :class="{ empty: !workspaceRoot }"
        :title="workspaceRoot ? `工作空间：${workspaceRoot}（点击更换）` : '选择一个文件夹作为工作空间'"
        @click="chooseWorkspace"
      >{{ workspaceRoot ? workspaceName : "选择文件夹" }}</button>
      <button type="button" class="workspace-head-close" :title="`收起工作区（${shortcut('Ctrl+Shift+W')}）`" @click="close">×</button>
    </header>

    <section v-if="!tab" class="workspace-launcher" aria-label="打开工作区工具">
      <button v-for="item in tabs" :key="item.id" type="button" @click="activate(item.id)">
        <span class="workspace-icon">{{ item.icon }}</span>
        <strong>{{ item.label }}</strong>
        <small>{{ item.shortcut }}</small>
      </button>
    </section>

    <template v-else>
      <section v-show="tab === 'explorer'" class="workspace-pane explorer-pane">
        <ProjectExplorer v-if="session.projectExplorer" />
        <div v-else class="workspace-empty">
          <span>▣</span>
          <strong>选择一个工作空间</strong>
          <p>选择本地文件夹后，可以在这里浏览、编辑文件并查看 Git 改动。</p>
          <button type="button" @click="chooseWorkspace">选择文件夹</button>
        </div>
      </section>

      <section v-show="tab === 'review'" class="workspace-pane review-pane">
        <header class="pane-toolbar"><div><strong>Review</strong><small>{{ review?.branch || '选择工作区后读取改动' }}</small></div><button type="button" :disabled="!workspaceRoot || reviewLoading" @click="loadReview">{{ reviewLoading ? '读取中' : '刷新' }}</button></header>
        <div v-if="!workspaceRoot" class="workspace-empty"><span>▣</span><strong>选择一个工作区</strong><p>选择文件夹后，可以查看 Git 分支和改动。</p><button type="button" @click="chooseWorkspace">选择文件夹</button></div>
        <div v-else-if="reviewError" class="workspace-notice error">{{ reviewError }}</div>
        <div v-else-if="reviewLoading" class="workspace-empty compact">正在读取 Git 状态…</div>
        <div v-else-if="review && !review.files.length" class="workspace-empty compact"><span>✓</span><strong>工作区没有未提交改动</strong></div>
        <div v-else class="review-list">
          <button v-for="file in review?.files" :key="file.path" type="button" class="review-row" @click="openReviewFile(file.path)"><span :class="['review-state', file.indexStatus !== ' ' ? 'staged' : '']">{{ file.indexStatus }}{{ file.worktreeStatus }}</span><span>{{ file.path }}</span></button>
        </div>
      </section>

      <section v-show="tab === 'terminal'" class="workspace-pane terminal-pane">
        <header class="pane-toolbar"><div><strong>Terminal</strong><small>{{ terminalCwd }}</small></div></header>
        <div ref="terminalScreen" class="terminal-screen" @pointerdown.prevent="focusTerminal"><pre class="terminal-output">{{ terminalTranscript }}</pre><form class="terminal-commandline" @submit.prevent="runCommand"><span>{{ terminalPrompt }}</span><input ref="terminalInput" v-model="command" :disabled="!session.sessionId || launching" aria-label="PowerShell 命令" spellcheck="false" /></form></div>
        <p v-if="terminalError" class="workspace-notice error">{{ terminalError }}</p>
        <button v-if="latestTerminal && !latestTerminal.exited" class="terminal-stop" type="button" @click="session.killLiveTerminal(latestTerminal.terminalId)">停止当前命令</button>
      </section>

      <section v-show="tab === 'browser'" class="workspace-pane browser-pane">
        <div class="browser-tabs"><div v-for="item in browserTabs" :key="item.id" class="browser-tab" :class="{ active: activeBrowserId === item.id }"><button type="button" @click="selectBrowserTab(item.id)">◎ {{ item.title }}</button><button class="browser-tab-close" type="button" :title="`关闭 ${item.title}`" @click="closeBrowserTab(item.id)">×</button></div><button class="browser-new-tab" type="button" title="新标签页" @click="newBrowserTab">＋</button></div>
        <form class="browser-address" @submit.prevent="navigate"><button type="button" title="重新加载" @click="reloadBrowser">↻</button><input v-model="browserDraft" placeholder="搜索或输入 URL" /><button type="submit" title="打开">↗</button></form>
        <p v-if="browserError" class="workspace-notice">{{ browserError }}</p>
        <div v-if="!browserUrl" class="workspace-empty native-browser"><span>◎</span><strong>开始浏览</strong><p>在上方输入网址，在工作区内打开网页。</p></div>
        <div v-else ref="browserHost" class="native-browser-host" aria-label="内置浏览器内容" />
      </section>
    </template>
  </aside>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { browserEmbedHide, browserEmbedShow, terminalRunUser, workspaceReview } from "../lib/ipc";
import type { ReviewSnapshot } from "../lib/types";
import {
  useSessionStore,
  WORKBENCH_DEFAULT_WIDTH,
  WORKBENCH_MAX_WIDTH,
  WORKBENCH_MIN_WIDTH,
  type WorkbenchTool,
} from "../stores/session";
import { hasPrimaryModifier, shortcut } from "../lib/platform";
import PaneResizer from "./PaneResizer.vue";
import ProjectExplorer from "./ProjectExplorer.vue";

const session = useSessionStore();

// 开关与当前工具都住在 store 里：标题栏的 ◧、视图菜单、快捷键和这里
// 操作的是同一份状态，不再用 prop / emit 在父子之间转手。
const visible = computed(() => session.workbenchVisible);
const tab = computed(() => session.workbenchTool);

const tabs: { id: WorkbenchTool; label: string; icon: string; shortcut: string }[] = [
  { id: "explorer", label: "资源管理器", icon: "▣", shortcut: shortcut("Ctrl+P") },
  { id: "review", label: "审查", icon: "▤", shortcut: shortcut("Ctrl+Shift+G") },
  { id: "terminal", label: "终端", icon: "›_", shortcut: shortcut("Ctrl+`") },
  { id: "browser", label: "浏览器", icon: "◎", shortcut: shortcut("Ctrl+T") },
];
const currentTool = computed(() => tabs.find((item) => item.id === tab.value) ?? tabs[0]!);

// 宽度用 inline style 落地，页面里剩下的百分比约束会让位给用户拖出来的值；
// 上限兜一手 60%，避免窄窗口下面板把对话区挤没。
const paneStyle = computed(() => ({
  flexBasis: `${session.workbenchWidth}px`,
  width: `${session.workbenchWidth}px`,
  maxWidth: "60%",
}));

const command = ref(""); const launching = ref(false); const terminalError = ref(""); const terminalInput = ref<HTMLInputElement | null>(null); const terminalScreen = ref<HTMLElement | null>(null); const terminalHistory = ref<string[]>([]); const clearedTerminalIds = ref(new Set<string>()); const terminalDirectory = ref("");
const review = ref<ReviewSnapshot | null>(null); const reviewLoading = ref(false); const reviewError = ref("");
type BrowserTab = { id: number; title: string; url: string; draft: string };
const browserTabs = ref<BrowserTab[]>([{ id: 1, title: "新标签页", url: "", draft: "" }]); const activeBrowserId = ref(1); const browserError = ref(""); const browserHost = ref<HTMLElement | null>(null);

// 资源管理器搬进面板后，它的工作空间就是面板的工作空间：审查与终端都跟着它走。
const workspaceRoot = computed(() => session.projectExplorer?.workspace || session.selectedFolder || session.active?.prefs.workspace || session.active?.workspace || "");
const workspaceName = computed(() => workspaceRoot.value.split(/[\\/]/).filter(Boolean).pop() || "工作空间");
const terminalCwd = computed(() => terminalDirectory.value || workspaceRoot.value || "未选择工作区");
const terminalPrompt = computed(() => {
  const path = terminalCwd.value.replace(/[\\/]+$/, "");
  const name = path.split(/[\\/]/).filter(Boolean).pop() || terminalCwd.value;
  return `PS ${name}>`;
});
const sessionTerminals = computed(() => Object.values(session.terminals).filter((item) => item.sessionId === session.sessionId));
const latestTerminal = computed(() => sessionTerminals.value[sessionTerminals.value.length - 1]);
const terminalWelcome = computed(() => workspaceRoot.value
  ? "Windows PowerShell\nCopyright (C) Microsoft Corporation. All rights reserved.\n\n"
  : "先选择文件夹，再在这里打开本地终端。\n");
const terminalTranscript = computed(() => {
  const output = sessionTerminals.value.filter((term) => !clearedTerminalIds.value.has(term.terminalId)).map((term) => term.text).filter(Boolean).join("");
  return `${terminalWelcome.value}${terminalHistory.value.join("")}${output}`;
});
const activeBrowser = computed(() => browserTabs.value.find((item) => item.id === activeBrowserId.value) ?? browserTabs.value[0]!);
const browserDraft = computed({ get: () => activeBrowser.value?.draft ?? "", set: (value: string) => { if (activeBrowser.value) activeBrowser.value.draft = value; } });
const browserUrl = computed(() => activeBrowser.value?.url ?? "");

function focusTerminal() { terminalInput.value?.focus(); void nextTick(() => { terminalInput.value?.focus(); if (terminalScreen.value) terminalScreen.value.scrollTop = terminalScreen.value.scrollHeight; }); }
function activate(next: WorkbenchTool) { session.selectWorkbenchTool(next); if (next === "review" && workspaceRoot.value && !review.value) void loadReview(); if (next === "browser" && browserUrl.value) void showEmbeddedBrowser(); else void browserEmbedHide(); if (next === "terminal") focusTerminal(); }
function backToLauncher() { session.selectWorkbenchTool(null); void browserEmbedHide(); }
function close() { void browserEmbedHide(); session.closeWorkbench(); }
function openTool(next: WorkbenchTool) { session.openWorkbench(next); if (next === "review" && workspaceRoot.value && !review.value) void loadReview(); if (next === "terminal") focusTerminal(); }
function onWorkspaceTool(event: Event) { const next = (event as CustomEvent<WorkbenchTool>).detail; if (tabs.some((item) => item.id === next)) openTool(next); }
// 头部的文件夹名和空态里的按钮都是「换一个工作空间」：直接调起目录选择器。
// 已有项目之间的切换由资源管理器自己的项目切换器负责，这里不重复。
// 用 pickWorkbenchFolder 而不是 newProject：后者会把用户带进左栏的「空间」语境，
// 而这个按钮的语境是「给这个面板换一个目录」。
function chooseWorkspace() { void session.pickWorkbenchFolder(); }
async function loadReview() { if (!workspaceRoot.value) return; reviewLoading.value = true; reviewError.value = ""; try { review.value = await workspaceReview(workspaceRoot.value); } catch (err) { reviewError.value = String(err); } finally { reviewLoading.value = false; } }
// 审查里点一条改动：落到资源管理器，由它按实际文件树展开（下一步再接「定位到该文件」）。
function openReviewFile(path: string) { void path; activate("explorer"); }
function parentDirectory(path: string) { const normalized = path.replace(/[\\/]+$/, ""); const index = Math.max(normalized.lastIndexOf("\\"), normalized.lastIndexOf("/")); return index > 2 ? normalized.slice(0, index) : normalized; }
async function runCommand() { const text = command.value.trim(); if (!session.sessionId || !text) return; command.value = ""; terminalError.value = ""; if (/^(cls|clear)$/i.test(text)) { terminalHistory.value = []; clearedTerminalIds.value = new Set(sessionTerminals.value.map((term) => term.terminalId)); focusTerminal(); return; } const cd = text.match(/^(?:cd|chdir)\s+(.+)$/i); if (cd?.[1]?.trim() === "..") { const previous = terminalCwd.value; const previousPrompt = terminalPrompt.value; terminalDirectory.value = parentDirectory(previous); terminalHistory.value.push(`${previousPrompt} ${text}\n`); focusTerminal(); return; } launching.value = true; terminalHistory.value.push(`${terminalPrompt.value} ${text}\n`); try { await terminalRunUser(session.sessionId, text, terminalCwd.value); } catch (err) { terminalError.value = String(err); } finally { launching.value = false; focusTerminal(); } }
async function showEmbeddedBrowser() { await nextTick(); const host = browserHost.value; if (!host || !browserUrl.value || tab.value !== "browser") return; const rect = host.getBoundingClientRect(); if (rect.width < 20 || rect.height < 20) return; try { browserError.value = ""; await browserEmbedShow(browserUrl.value, rect.left, rect.top, rect.width, rect.height); } catch (err) { browserError.value = err instanceof Error ? err.message : String(err); } }
function navigate() { const text = browserDraft.value.trim(); if (!text) return; const url = /^https?:\/\//i.test(text) ? text : `https://${text}`; try { const target = activeBrowser.value; if (!target) return; target.url = new URL(url).toString(); target.draft = target.url; target.title = new URL(target.url).hostname; void showEmbeddedBrowser(); } catch { browserError.value = "请输入有效的网址，例如 https://example.com"; } }
function reloadBrowser() { void showEmbeddedBrowser(); }
function newBrowserTab() { const id = Math.max(...browserTabs.value.map((item) => item.id)) + 1; browserTabs.value.push({ id, title: "新标签页", url: "", draft: "" }); activeBrowserId.value = id; browserError.value = ""; void browserEmbedHide(); }
function selectBrowserTab(id: number) { activeBrowserId.value = id; if (browserUrl.value) void showEmbeddedBrowser(); else void browserEmbedHide(); }
function closeBrowserTab(id: number) { const index = browserTabs.value.findIndex((item) => item.id === id); if (index < 0) return; browserTabs.value.splice(index, 1); if (!browserTabs.value.length) { browserTabs.value.push({ id: id + 1, title: "新标签页", url: "", draft: "" }); } activeBrowserId.value = browserTabs.value[Math.min(index, browserTabs.value.length - 1)]!.id; browserError.value = ""; if (browserUrl.value) void showEmbeddedBrowser(); else void browserEmbedHide(); }
function resizeBrowser() { if (tab.value === "browser" && browserUrl.value) void showEmbeddedBrowser(); }

// 竖列上写出来的快捷键要真能用，否则就是装饰。面板关着也生效：
// 按 Ctrl+P 的意图就是「把资源管理器叫出来」。
function onKeydown(event: KeyboardEvent) {
  if (!hasPrimaryModifier(event)) return;
  const key = event.key.toLowerCase();
  if (event.shiftKey) {
    if (key === "g") { event.preventDefault(); openTool("review"); }
    return;
  }
  if (key === "`" || key === "~") { event.preventDefault(); openTool("terminal"); return; }
  if (key === "t") { event.preventDefault(); openTool("browser"); return; }
  if (key === "p") { event.preventDefault(); openTool("explorer"); }
}

watch(() => workspaceRoot.value, () => { review.value = null; terminalDirectory.value = workspaceRoot.value; }, { immediate: true });
watch(() => session.sessionId, () => { terminalHistory.value = []; command.value = ""; clearedTerminalIds.value = new Set(); terminalDirectory.value = workspaceRoot.value; });
watch(terminalTranscript, () => { if (tab.value === "terminal") focusTerminal(); });
watch(visible, (open) => { if (!open) void browserEmbedHide(); });
onMounted(() => { window.addEventListener("resize", resizeBrowser); window.addEventListener("dandelion:workspace-tool", onWorkspaceTool); window.addEventListener("keydown", onKeydown); });
onBeforeUnmount(() => { window.removeEventListener("resize", resizeBrowser); window.removeEventListener("dandelion:workspace-tool", onWorkspaceTool); window.removeEventListener("keydown", onKeydown); void browserEmbedHide(); });
</script>
