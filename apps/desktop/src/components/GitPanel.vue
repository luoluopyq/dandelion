<template>
  <aside class="sidebar git-side-panel">
    <header class="side-head"><span>源代码管理</span><button type="button" title="刷新" :disabled="busy" @click="refresh">↻</button></header>
    <div v-if="!root" class="git-side-empty"><strong>选择项目以查看 Git</strong><button type="button" class="button-primary" @click="chooseRoot">选择项目</button></div>
    <div v-else-if="loading" class="git-side-empty">正在读取变更…</div>
    <div v-else-if="error" class="git-side-empty"><strong>无法读取 Git</strong><small>{{ error }}</small><button type="button" @click="refresh">重试</button></div>
    <template v-else-if="snapshot">
      <div v-if="!snapshot.gitAvailable" class="git-side-empty"><strong>未检测到 Git</strong><small>安装 Git 后重新打开应用。</small></div>
      <div v-else-if="!snapshot.repository" class="git-side-empty"><strong>这不是 Git 项目</strong><button type="button" class="button-primary" @click="initialize">初始化仓库</button></div>
      <template v-else>
        <section class="git-side-project"><strong :title="root">{{ projectName }}</strong><div class="git-branch-line"><span>⑂</span><select :value="snapshot.branch" @change="switchBranch(($event.target as HTMLSelectElement).value)"><option v-for="branch in snapshot.localBranches" :key="branch.name" :value="branch.name">{{ branch.current ? '✓ ' : '' }}{{ branch.name }}</option></select></div><small>{{ totalChanges ? `${totalChanges} 个文件有变更` : '工作区干净' }}</small><label v-if="snapshot.remoteBranches.length"><span>远程</span><select value="" @change="switchRemote(($event.target as HTMLSelectElement).value)"><option value="" disabled>检出远程分支…</option><option v-for="branch in snapshot.remoteBranches" :key="branch.name" :value="branch.name">{{ branch.name }}</option></select></label></section>
        <div class="git-commandbar"><button type="button" title="获取远程更新（Fetch）" :disabled="busy" @click="network('fetch')">↻</button><button type="button" title="拉取当前分支（Pull）" :disabled="busy" @click="network('pull')">↓</button><button type="button" title="推送当前分支（Push）" :disabled="busy" @click="network('push')">↑</button><span></span><button type="button" title="查看提交历史" @click="store.openGitReview({ root, view: 'history' })">◷</button></div>
        <section class="git-side-section git-change-tree"><div class="git-side-section-title"><span>变更</span><small>{{ totalChanges }}</small></div><details v-for="group in workingGroups" :key="group.folder" open><summary><span>⌄</span>{{ group.folder }} <small>{{ group.files.length }}</small></summary><button v-for="file in group.files" :key="`w:${file.path}`" type="button" class="git-file-row" @click="review(file.path, false)"><b>{{ file.status === '未跟踪' ? '?' : file.status }}</b><span :title="file.path">{{ fileName(file.path) }}</span><i title="暂存" @click.stop="stage(file.path)">＋</i></button></details><p v-if="!workingFiles.length" class="git-side-muted">没有待审阅的变更。</p></section>
        <section class="git-side-section git-change-tree"><div class="git-side-section-title"><span>已暂存</span><small>{{ snapshot.staged.length }}</small></div><details v-for="group in stagedGroups" :key="group.folder" open><summary><span>⌄</span>{{ group.folder }} <small>{{ group.files.length }}</small></summary><button v-for="file in group.files" :key="`s:${file.path}`" type="button" class="git-file-row" @click="review(file.path, true)"><b>{{ file.status }}</b><span :title="file.path">{{ fileName(file.path) }}</span><i title="取消暂存" @click.stop="unstage(file.path)">−</i></button></details><p v-if="!snapshot.staged.length" class="git-side-muted">暂存区为空。</p></section>
        <div class="git-side-actions"><button type="button" :disabled="!snapshot.staged.length" @click="store.openGitReview({ root, view: 'commit' })">提交已暂存的 {{ snapshot.staged.length }} 项</button></div>
      </template>
    </template>
  </aside>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { gitFetch, gitInit, gitPull, gitPush, gitSnapshot, gitStage, gitSwitchBranch } from "../lib/ipc";
import type { GitFile, GitSnapshot } from "../lib/types";
import { useSessionStore } from "../stores/session";
const store = useSessionStore();
const snapshot = ref<GitSnapshot | null>(null), loading = ref(false), busy = ref(false), error = ref("");
const root = computed(() => store.selectedFolder || store.active?.prefs.workspace || store.active?.workspace || "");
const projectName = computed(() => root.value.split(/[\\/]/).filter(Boolean).pop() || "项目");
const workingFiles = computed<GitFile[]>(() => [...(snapshot.value?.unstaged ?? []), ...(snapshot.value?.untracked ?? [])]);
const totalChanges = computed(() => workingFiles.value.length + (snapshot.value?.staged.length ?? 0));
function grouped(files: GitFile[]) { const groups = new Map<string, GitFile[]>(); for (const file of files) { const parts = file.path.split("/"); const folder = parts.length > 1 ? parts.slice(0, -1).join("/") : "根目录"; groups.set(folder, [...(groups.get(folder) ?? []), file]); } return [...groups].map(([folder, groupFiles]) => ({ folder, files: groupFiles })); }
const workingGroups = computed(() => grouped(workingFiles.value));
const stagedGroups = computed(() => grouped(snapshot.value?.staged ?? []));
function fileName(path: string) { return path.split("/").pop() || path; }
async function refresh() { if (!root.value) return; loading.value = true; error.value = ""; try { snapshot.value = await gitSnapshot(root.value); } catch (err) { snapshot.value = null; error.value = String(err); } finally { loading.value = false; } }
async function chooseRoot() { const path = await store.selectFolder(); if (path) await refresh(); }
async function initialize() { if (!root.value) return; await gitInit(root.value); await refresh(); }
function review(path: string, staged: boolean) { store.openGitReview({ root: root.value, view: "diff", path, staged }); }
async function stage(path: string) { if (!root.value) return; snapshot.value = await gitStage(root.value, path); }
async function unstage(path: string) { if (!root.value) return; snapshot.value = await gitStage(root.value, path, true); }
async function switchBranch(branch: string) { if (!root.value || !branch || branch === snapshot.value?.branch) return; try { snapshot.value = await gitSwitchBranch(root.value, branch); } catch (err) { error.value = String(err); } }
async function switchRemote(branch: string) { if (!root.value || !branch) return; try { snapshot.value = await gitSwitchBranch(root.value, branch, true); } catch (err) { error.value = String(err); } }
async function network(action: "fetch" | "pull" | "push") { if (!root.value) return; busy.value = true; error.value = ""; try { snapshot.value = await ({ fetch: gitFetch, pull: gitPull, push: gitPush }[action])(root.value); } catch (err) { error.value = String(err); } finally { busy.value = false; } }
function onGitRefresh() { void refresh(); }
watch(root, () => { void refresh(); }); onMounted(() => { window.addEventListener("dandelion:git-refresh", onGitRefresh); void refresh(); }); onBeforeUnmount(() => window.removeEventListener("dandelion:git-refresh", onGitRefresh));
</script>
