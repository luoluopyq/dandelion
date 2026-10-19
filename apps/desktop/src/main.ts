import { createPinia } from "pinia";
import { createApp } from "vue";
import App from "./App.vue";
import "./styles.css";

/*
 * 屏蔽 WebView2 自带的右键菜单（返回 / 刷新 / 另存为 / 打印 / 更多工具 / 检查）。
 *
 * 那套菜单是浏览器壳的，出现在桌面应用里读起来像「网页没加载完」：里面的「刷新」
 * 会把整个前端连正在跑的会话操作一起重来，「检查」把开发者工具摆在用户面前，
 * 「返回 / 另存为」在一个没有地址栏的窗口里也没有意义。
 *
 * 只拦非输入区域。输入框里的原生菜单是「剪切 / 复制 / 粘贴」，那是这个软件里少数
 * 必须留着的东西（聊天输入框右键粘贴很常用）；它和上面那套页面级菜单不是同一个。
 *
 * 用捕获阶段是必须的：各组件自己的右键菜单（会话行、资源管理器、分隔条）走的是
 * 各自的 @contextmenu，其中有的会 stopPropagation，冒泡阶段挂在 document 上的
 * 监听就收不到了。preventDefault 拦的是浏览器的**默认动作**，不会影响这些组件
 * 自己的菜单 —— 一个事件可以有多个监听器，只有默认行为会被拦下来。
 */
document.addEventListener(
  "contextmenu",
  (ev) => {
    const target = ev.target as HTMLElement | null;
    if (target?.closest("input, textarea, [contenteditable='true']")) return;
    ev.preventDefault();
  },
  { capture: true },
);

const app = createApp(App);
app.use(createPinia());
app.mount("#app");
