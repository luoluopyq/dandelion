<template>
  <nav class="feature-navigation" aria-label="页面导航">
    <button type="button" class="feature-navigation-back" @click="$emit('back')">
      <svg viewBox="0 0 20 20" width="16" height="16" fill="none" aria-hidden="true">
        <path d="m11.5 5-5 5 5 5M7 10h9" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
      <span>返回对话</span>
    </button>
  </nav>
</template>

<script setup lang="ts">
defineEmits<{ back: [] }>();
</script>

<style scoped>
.feature-navigation {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  min-height: 44px;
  padding: 6px 16px;
  box-sizing: border-box;
  border-bottom: 1px solid var(--border);
  background: var(--bg);
}
.feature-navigation-back {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-height: 32px;
  padding: 6px 8px;
  border: 0;
  border-radius: 6px;
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-size: 12px;
  line-height: 20px;
  white-space: nowrap;
  cursor: pointer;
}
.feature-navigation-back:hover { background: var(--bg-3); color: var(--text); }
.feature-navigation-back:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.feature-navigation-back:active { background: color-mix(in srgb, var(--accent) 12%, var(--bg)); }
</style>
