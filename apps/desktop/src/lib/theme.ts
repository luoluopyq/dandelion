/**
 * 主题状态。浅色为默认（对齐 Dyad），深色为可选。
 *
 * 生效方式是给 <html> 加/去 `.dark` —— styles.css 里两套 token 分别挂在
 * `:root` 和 `.dark` 上，所以切 class 就等于整套换色，不需要组件参与。
 * 选择持久化到 localStorage；读不到就跟随系统。
 */
import { ref, watch } from "vue";

export type Theme = "light" | "dark";

const KEY = "dandelion.theme";

function initial(): Theme {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // 隐私模式或禁用站点数据时 localStorage 访问本身会抛，回落到系统偏好。
  }
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export const theme = ref<Theme>(initial());

function apply(next: Theme) {
  document.documentElement.classList.toggle("dark", next === "dark");
}

watch(theme, (next) => {
  apply(next);
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // 存不下就只在本次会话生效，不影响显示。
  }
}, { immediate: true });

export function toggleTheme() {
  theme.value = theme.value === "dark" ? "light" : "dark";
}
