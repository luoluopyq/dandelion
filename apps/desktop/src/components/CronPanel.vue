<template>
  <section class="cron-page">
    <header class="cron-header">
      <div><p class="eyebrow">自动化</p><h1>定时任务</h1><p class="cron-subtitle">在指定时间启动独立任务；结果保存在对应项目的会话中。</p></div>
      <div class="cron-header-actions"><button type="button" class="button-quiet" :disabled="loading" @click="refresh">刷新</button><button type="button" class="button-primary" @click="composerOpen = true">＋ 新建任务</button></div>
    </header>

    <section v-if="composerOpen" class="cron-composer" aria-label="新建定时任务">
      <div class="cron-composer-heading"><div><h2>新建定时任务</h2><p>先描述要完成的事，再设置执行频率。</p></div><button type="button" class="icon-button" title="关闭" @click="composerOpen = false">×</button></div>
      <label class="cron-field cron-field-wide"><span>任务内容</span><textarea v-model="prompt" rows="3" placeholder="例如：每天上午汇总项目的未提交变更，并给出风险提示" autofocus /></label>
      <label class="cron-field"><span>执行频率</span><select v-model="preset" @change="applyPreset"><option value="30s">每 30 秒（测试）</option><option value="1m">每分钟</option><option value="1h">每小时</option><option value="daily">每天上午 9:00</option><option value="custom">自定义</option></select></label>
      <label class="cron-field"><span>时间表达式</span><input v-model="expr" placeholder="例如 1h 或 0 9 * * *" /></label>
      <label class="cron-field"><span>错过执行时</span><select v-model="policy"><option value="skip">跳过，等待下次</option><option value="run_once">恢复后补跑一次</option></select></label>
      <div class="cron-composer-footer"><div><button type="button" class="button-quiet" @click="composerOpen = false">取消</button><button type="button" class="button-primary" :disabled="!canCreate" @click="create">创建任务</button></div></div>
    </section>

    <nav class="cron-filter" aria-label="任务筛选"><button type="button" :class="{ on: filter === 'all' }" @click="filter = 'all'">全部 <span>{{ store.cronJobs.length }}</span></button><button type="button" :class="{ on: filter === 'active' }" @click="filter = 'active'">运行中 <span>{{ activeCount }}</span></button><button type="button" :class="{ on: filter === 'paused' }" @click="filter = 'paused'">已暂停 <span>{{ pausedCount }}</span></button><button type="button" :class="{ on: filter === 'runs' }" @click="showRuns">运行记录 <span>{{ runs.length }}</span></button></nav>

    <template v-if="filter === 'runs'">
      <div v-if="runsLoading" class="cron-empty">正在读取运行记录…</div>
      <div v-else-if="!runs.length" class="cron-empty"><strong>还没有运行记录</strong><p>任务到点执行后会出现在这里，点击可以查看那次执行做了什么。</p></div>
      <section v-else class="cron-runs" aria-label="运行记录">
        <button v-for="item in runs" :key="item.sessionId" type="button" class="cron-run" :title="`打开这次执行的会话（${item.sessionId}）`" @click="openRun(item.sessionId)">
          <span class="cron-run-job">{{ jobTitle(item.jobPrompt) }}</span>
          <span class="cron-run-time">{{ localTime(item.startedAt) }}</span>
          <span class="cron-run-state" :class="{ pending: !item.finished }">{{ item.finished ? '已完成' : '未产出回复' }}</span>
          <span class="cron-run-summary">{{ item.summary ? oneLine(item.summary) : '（无回复内容）' }}</span>
          <span class="cron-run-go">查看 →</span>
        </button>
      </section>
    </template>

    <div v-else-if="loading" class="cron-empty">正在读取定时任务…</div>
    <div v-else-if="!filteredJobs.length" class="cron-empty"><strong>{{ filter === 'all' ? '还没有定时任务' : '这里没有任务' }}</strong><p>{{ filter === 'all' ? '把重复的检查、整理或提醒交给蒲公英自动完成。' : '切换筛选条件，或新建一个任务。' }}</p><button v-if="filter === 'all'" type="button" class="button-primary" @click="composerOpen = true">新建定时任务</button></div>
    <section v-else class="cron-list" aria-label="定时任务列表"><article v-for="job in filteredJobs" :key="job.id" class="cron-card"><div class="cron-card-main"><div class="cron-status" :class="{ paused: !job.enabled }"><span></span>{{ job.enabled ? '运行中' : '已暂停' }}</div><h2 :title="job.prompt">{{ jobTitle(job.prompt) }}</h2><p class="cron-prompt">{{ job.prompt }}</p><dl class="cron-meta"><div><dt>频率</dt><dd>{{ readableExpr(job.expr) }}</dd></div><div><dt>下次执行</dt><dd>{{ job.enabled ? localTime(job.nextRun) : '已暂停' }}</dd></div><div><dt>最近运行</dt><dd>{{ job.lastRun ? localTime(job.lastRun) : '尚未运行' }}</dd></div></dl></div><div class="cron-card-actions"><button type="button" class="button-quiet" @click="run(job.id)">立即运行</button><button type="button" class="button-quiet" @click="job.enabled ? pause(job.id) : resume(job.id)">{{ job.enabled ? '暂停' : '恢复' }}</button><button type="button" class="button-danger" @click="remove(job.id)">删除</button></div></article></section>
    <p v-if="error" class="cron-error">{{ error }}</p>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useSessionStore } from "../stores/session";
import type { CronRunRow } from "../lib/types";
const store = useSessionStore();
const loading = ref(false), error = ref(""), composerOpen = ref(false), filter = ref<"all" | "active" | "paused" | "runs">("all"), prompt = ref(""), expr = ref("1h"), preset = ref("1h"), policy = ref("skip");
type RunItem = CronRunRow & { jobId: string; jobPrompt: string };
const runs = ref<RunItem[]>([]);
const runsLoading = ref(false);
const activeCount = computed(() => store.cronJobs.filter((job) => job.enabled).length);
const pausedCount = computed(() => store.cronJobs.length - activeCount.value);
const filteredJobs = computed(() => store.cronJobs.filter((job) => filter.value === "all" || (filter.value === "active" ? job.enabled : !job.enabled)));
const canCreate = computed(() => !!prompt.value.trim() && !!expr.value.trim());
async function refresh() { loading.value = true; error.value = ""; try { await store.refreshCron(); } catch (err) { error.value = String(err); } finally { loading.value = false; } }
function applyPreset() { if (preset.value === "daily") expr.value = "0 9 * * *"; else if (preset.value !== "custom") expr.value = preset.value; }
async function create() { if (!canCreate.value) return; error.value = ""; try { await store.addCron({ expr: expr.value.trim(), prompt: prompt.value.trim(), missedPolicy: policy.value }); composerOpen.value = false; prompt.value = ""; await refresh(); } catch (err) { error.value = String(err); } }
async function run(id: string) { try { await store.runCron(id); await refresh(); if (filter.value === "runs") await loadRuns(); } catch (err) { error.value = String(err); } }
async function pause(id: string) { try { await store.pauseCron(id); await refresh(); } catch (err) { error.value = String(err); } }
async function resume(id: string) { try { await store.resumeCron(id); await refresh(); } catch (err) { error.value = String(err); } }
async function remove(id: string) { if (!window.confirm("删除这个定时任务？已有的任务会话不会删除。")) return; try { await store.removeCron(id); await refresh(); } catch (err) { error.value = String(err); } }
function jobTitle(value: string) { return value.trim().split(/\r?\n/)[0].slice(0, 44) || "未命名定时任务"; }

// 运行记录汇总所有任务的历次执行，按开始时间倒序。
async function loadRuns() {
  runsLoading.value = true;
  error.value = "";
  try {
    const jobs = store.cronJobs;
    const perJob = await Promise.all(
      jobs.map(async (job) => {
        const rows = await store.cronRuns(job.id, 20).catch(() => [] as CronRunRow[]);
        return rows.map((row) => ({ ...row, jobId: job.id, jobPrompt: job.prompt }));
      }),
    );
    runs.value = perJob.flat().sort((a, b) => b.startedAt.localeCompare(a.startedAt));
  } catch (err) {
    error.value = String(err);
  } finally {
    runsLoading.value = false;
  }
}
async function showRuns() { filter.value = "runs"; await loadRuns(); }
function openRun(sessionId: string) { void store.openSession(sessionId); }
function oneLine(value: string) { const line = value.trim().replace(/\s+/g, " "); return line.length > 96 ? `${line.slice(0, 96)}…` : line; }
function localTime(value: string) {
  const at = new Date(value);
  if (Number.isNaN(at.getTime())) return value;
  return at.toLocaleString();
}
function readableExpr(value: string) { const labels: Record<string, string> = { "30s": "每 30 秒", "1m": "每分钟", "1h": "每小时", "0 9 * * *": "每天上午 9:00" }; return labels[value] || value; }
onMounted(() => { void refresh(); });
</script>
