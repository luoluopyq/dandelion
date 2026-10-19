<template>
  <div class="g5e-panel">
    <p class="hint">
      高级规格工作台：这里会维护工作区内的 SPEC/PLAN 文件；它不同于主输入区的 Ask / Plan / Auto 对话模式。
    </p>
    <label class="block">
      规格流程
      <select v-model="mode">
        <option value="ask">生成规格草案</option>
        <option value="plan">生成规格与计划</option>
        <option value="auto">生成并执行计划</option>
      </select>
    </label>
    <label class="block">
      Prompt
      <textarea v-model="prompt" rows="3" placeholder="用自然语言描述目标" />
    </label>
    <div class="row-btns">
      <button type="button" :disabled="busy" @click="onStart">开始</button>
      <button type="button" :disabled="!specId || busy" @click="onShow">Show</button>
      <button type="button" :disabled="!specId || busy" @click="onDiff">Diff</button>
      <button type="button" :disabled="!specId || busy" @click="onApprove">Approve</button>
      <button type="button" :disabled="!specId || busy" @click="onRun">Run</button>
    </div>
    <label class="block" v-if="specId">
      修订说明
      <textarea v-model="revise" rows="2" placeholder="可选：修订后点 Revise" />
      <button type="button" :disabled="busy" @click="onRevise">Revise</button>
    </label>
    <pre v-if="output" class="json">{{ output }}</pre>
    <p v-if="store.panelError" class="err">{{ store.panelError }}</p>
  </div>
</template>

<script setup lang="ts">
import { ref } from "vue";
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
const mode = ref<"ask" | "plan" | "auto">("ask");
const prompt = ref("");
const revise = ref("");
const specId = ref("");
const output = ref("");
const busy = ref(false);

async function onStart() {
  busy.value = true;
  try {
    const snap = await store.specStart(mode.value, prompt.value);
    specId.value = snap?.specId ?? "";
    output.value = JSON.stringify(snap, null, 2);
  } finally {
    busy.value = false;
  }
}

async function onShow() {
  busy.value = true;
  try {
    output.value = JSON.stringify(await store.specShow(specId.value), null, 2);
  } finally {
    busy.value = false;
  }
}

async function onDiff() {
  busy.value = true;
  try {
    output.value = JSON.stringify(await store.specDiff(specId.value), null, 2);
  } finally {
    busy.value = false;
  }
}

async function onApprove() {
  busy.value = true;
  try {
    output.value = JSON.stringify(await store.specApprove(specId.value), null, 2);
  } finally {
    busy.value = false;
  }
}

async function onRevise() {
  busy.value = true;
  try {
    output.value = JSON.stringify(await store.specRevise(specId.value, revise.value), null, 2);
  } finally {
    busy.value = false;
  }
}

async function onRun() {
  busy.value = true;
  try {
    const sid = await store.specRun(specId.value, prompt.value || "execute approved plan");
    output.value = `run session: ${sid}`;
  } finally {
    busy.value = false;
  }
}
</script>
