<template>
  <!-- agent_loop 母题：三个节点 + 顺时针箭头弧，和 apps/desktop/src-tauri/icons/icon.svg
       同一套几何。整枚标记只描 currentColor，所以既能落在实心品牌色底上（侧栏 logo），
       也能落在纯色底上（欢迎页），不需要两套资源，也不用 ☸ 这种靠字体兜底的字形
       —— 那个字形在不同平台渲染成完全不同的东西。 -->
  <svg class="brand-mark" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
    <g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">
      <path d="M14.12 6.17A6.2 6.2 0 0 1 18.11 13.08" />
      <path d="M15.99 16.75A6.2 6.2 0 0 1 8.01 16.75" />
      <path d="M5.89 13.08A6.2 6.2 0 0 1 9.88 6.17" />
    </g>
    <g fill="currentColor">
      <circle cx="12" cy="5.8" r="1.85" />
      <circle cx="17.37" cy="15.1" r="1.85" />
      <circle cx="6.63" cy="15.1" r="1.85" />
    </g>
  </svg>
</template>

<script setup lang="ts">
defineOptions({ name: "BrandMark" });
</script>
