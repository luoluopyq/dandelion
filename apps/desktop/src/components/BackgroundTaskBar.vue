<template>
  <section v-if="jobs.length" class="background-tasks" aria-label="后台任务">
    <button class="background-summary" type="button" :aria-expanded="expanded" @click="expanded = !expanded">
      <span class="background-dot" :class="{ stopped: !runningCount }" aria-hidden="true" />
      <strong>{{ runningCount ? `后台运行 ${runningCount} 个` : "后台任务已结束" }}</strong>
      <span class="background-command" :title="headline.command">{{ headline.command }}</span>
      <span v-if="runningCount && headline.startedAt" class="background-time">{{ elapsed(headline.startedAt) }}</span>
      <span class="background-chevron" aria-hidden="true">{{ expanded ? "⌃" : "⌄" }}</span>
    </button>
    <div v-if="expanded" class="background-details">
      <article v-for="job in jobs" :key="job.terminalId" class="background-job">
        <div class="background-job-head">
          <div class="background-job-name">
            <strong :title="job.command">{{ job.command }}</strong>
            <small :title="job.cwd">{{ job.cwd }}</small>
          </div>
          <span class="background-job-status" :class="job.exited && job.exitCode != null && job.exitCode !== 0 ? 'failed' : job.status ?? (job.exited ? 'exited' : 'running')">{{ statusLabel(job) }}<small v-if="!job.exited && job.startedAt">{{ elapsed(job.startedAt) }}</small></span>
        </div>
        <pre class="background-output">{{ latestOutput(job.text) }}</pre>
        <div class="background-actions">
          <button type="button" @click="session.openLiveTerminal(job.terminalId)">查看终端</button>
          <button v-if="!job.exited" type="button" :disabled="stoppingIds.has(job.terminalId)" @click="stop(job.terminalId)">{{ stoppingIds.has(job.terminalId) ? "正在停止…" : "停止进程" }}</button>
          <button v-else type="button" @click="dismissedIds.add(job.terminalId)">移除记录</button>
        </div>
      </article>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { latestOutput } from "../lib/terminal-preview";
import type { LiveTerminal } from "../lib/types";
import { useSessionStore } from "../stores/session";

const session = useSessionStore();
const expanded = ref(false);
const dismissedIds = ref(new Set<string>());
const stoppingIds = ref(new Set<string>());
const now = ref(Date.now());
const hasBrowserTimers = typeof window !== "undefined" && typeof window.setInterval === "function";
const timer = hasBrowserTimers ? window.setInterval(() => { now.value = Date.now(); }, 1000) : null;
onBeforeUnmount(() => { if (timer != null) window.clearInterval(timer); });

watch(() => session.sessionId, (id) => {
  expanded.value = false;
  if (id) void session.refreshBackgroundProcesses(id);
}, { immediate: hasBrowserTimers });

const jobs = computed(() => Object.values(session.terminals)
  .filter((job) => job.sessionId === session.sessionId && job.background && !dismissedIds.value.has(job.terminalId))
  .sort((a, b) => Number(a.exited) - Number(b.exited) || (b.startedAt ?? 0) - (a.startedAt ?? 0)));
const runningCount = computed(() => jobs.value.filter((job) => !job.exited).length);
const headline = computed(() => jobs.value[0]);

function elapsed(startedAt: number): string {
  const seconds = Math.max(0, Math.floor((now.value - startedAt) / 1000));
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${String(seconds % 60).padStart(2, "0")} 秒`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor((seconds % 3600) / 60)} 分`;
}

function statusLabel(job: LiveTerminal): string {
  // 事件丢帧重同步后仍没有退出帧：任务已结束却看不到结局，如实标注，
  // 不许继续显示成「运行中」骗人（停止按钮保留，用户可自行了断）。
  if (job.unverified) return "状态未确认";
  if (!job.exited) return "运行中";
  if (job.status === "killed") return "已停止";
  if (job.exitCode === 0) return "已结束";
  if (job.exitCode != null) return `退出码 ${job.exitCode}`;
  return "已结束";
}

async function stop(id: string) {
  stoppingIds.value = new Set([...stoppingIds.value, id]);
  try { await session.killLiveTerminal(id); }
  finally {
    const next = new Set(stoppingIds.value);
    next.delete(id);
    stoppingIds.value = next;
  }
}
</script>

<style scoped>
.background-tasks { flex: 0 0 auto; width: min(calc(100% - 28px), var(--chat-col)); margin: 0 auto 6px; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--bg-2); overflow: hidden; }
.background-summary { display: flex; align-items: center; gap: 9px; width: 100%; min-height: 38px; border: 0; padding: 7px 12px; background: transparent; color: var(--text); text-align: left; font: inherit; font-size: var(--fs-xs); cursor: pointer; }
.background-summary:hover { background: var(--hover); }
.background-summary:focus-visible, .background-actions button:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.background-summary strong { flex: 0 0 auto; font-weight: 600; }
.background-dot { flex: 0 0 auto; width: 7px; height: 7px; border-radius: 50%; background: var(--ok); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 14%, transparent); }
.background-dot.stopped { background: var(--muted); box-shadow: none; }
.background-command { min-width: 0; overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
.background-time { margin-left: auto; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
.background-chevron { margin-left: 3px; color: var(--muted); font-size: var(--fs-base); }
.background-details { max-height: min(40vh, 330px); overflow: auto; border-top: 1px solid var(--border); }
.background-job { padding: 12px; border-top: 1px solid var(--border); }
.background-job:first-child { border-top: 0; }
.background-job-head { display: flex; justify-content: space-between; gap: 12px; align-items: flex-start; }
.background-job-name { display: grid; gap: 3px; min-width: 0; }
.background-job-name strong { overflow: hidden; color: var(--text); font-size: var(--fs-xs); text-overflow: ellipsis; white-space: nowrap; }
.background-job-name small { overflow: hidden; color: var(--muted); font-size: var(--fs-2xs); text-overflow: ellipsis; white-space: nowrap; }
.background-job-status { flex: 0 0 auto; color: var(--muted); font-size: var(--fs-2xs); }
.background-job-status small { display: block; margin-top: 3px; color: var(--muted); font-size: var(--fs-2xs); font-variant-numeric: tabular-nums; text-align: right; }
.background-job-status.running { color: var(--ok); }
.background-job-status.failed, .background-job-status.timeout { color: var(--danger); }
.background-output { margin: 10px 0 0; padding: 8px 10px; border-radius: var(--radius-md); background: var(--bg-3); color: var(--muted); font-family: var(--mono); font-size: var(--fs-2xs); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.background-actions { display: flex; justify-content: flex-end; gap: 7px; margin-top: 10px; }
.background-actions button { border: 1px solid var(--border); border-radius: var(--radius-md); padding: 5px 9px; background: var(--surface); color: var(--text); font: inherit; font-size: var(--fs-2xs); cursor: pointer; }
.background-actions button:hover:not(:disabled) { background: var(--hover); }
.background-actions button:disabled { opacity: .5; cursor: default; }
@media (max-width: 520px) { .background-command { display: none; } .background-details { max-height: 32vh; } }
</style>
