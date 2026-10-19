<template>
  <div class="g5e-panel">
    <p class="hint">
      启用列表走 Settings 键。空 = 无钩子。顺序：门 1 → pre_tool_call → 门 3 → 门 2 → handler → post_tool_call。
      <strong>禁止</strong>在钩子里再调模型、执行工具或开第二套循环。
    </p>
    <p class="muted">已知内置 id：deny_named_tool（pre_tool_call 拒绝 write_file）。未知 id 会被忽略。</p>
    <div class="row-btns">
      <button type="button" @click="store.openHooksDir()">打开 hooks 目录</button>
    </div>
    <p class="muted">目录在 data_dir/hooks。超时与启用列表用上面的键，改完对下一轮 prompt 生效。</p>
  </div>
</template>

<script setup lang="ts">
import { useSessionStore } from "../stores/session";

const store = useSessionStore();
</script>
