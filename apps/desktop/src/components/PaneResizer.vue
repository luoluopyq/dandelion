<template>
  <!-- 分栏拖拽条。左右两侧各放一个（左栏 / 右侧工作台），逻辑完全共用：
     这两个分界线必须能横着拖、双击回默认值、用方向键微调，行为不能分叉。

     它只调宽度，不碰别的：拿到的是 modelValue，吐回去的是新宽度，
     怎么存由父级决定（拖拽过程不落盘、松手才落盘）。 -->
  <div
    class="pane-resizer"
    :class="{ dragging }"
    role="separator"
    aria-orientation="vertical"
    :aria-label="label"
    :aria-valuenow="modelValue"
    :aria-valuemin="min"
    :aria-valuemax="max"
    :aria-valuetext="`${modelValue} 像素`"
    tabindex="0"
    @pointerdown="startDrag"
    @dblclick="reset"
    @keydown="onKeydown"
  />
</template>

<script setup lang="ts">
import { ref } from "vue";

const props = withDefaults(
  defineProps<{
    /** 被拖的那个面板的当前宽度（px）。 */
    modelValue: number;
    min: number;
    max: number;
    /** 双击复位到这个宽度。 */
    reset: number;
    /** 无障碍名称，例如「调整左栏宽度」。 */
    label: string;
    /** 面板贴右边缘时为 true：指针往左拖才是变宽。 */
    inverted?: boolean;
    /** 方向键步长。 */
    step?: number;
  }>(),
  { inverted: false, step: 16 },
);

const emit = defineEmits<{
  "update:modelValue": [width: number];
  /** 拖动结束 / 键盘调整完成，父级可以落盘了。 */
  commit: [width: number];
}>();

const dragging = ref(false);

function clamp(value: number): number {
  return Math.round(Math.min(props.max, Math.max(props.min, value)));
}

function apply(width: number, commit: boolean) {
  const next = clamp(width);
  emit("update:modelValue", next);
  if (commit) emit("commit", next);
}

function startDrag(event: PointerEvent) {
  // 只认左键：右键和中键拖出来的分栏在 Windows 上没有先例。
  if (event.button !== 0) return;
  event.preventDefault();

  const direction = props.inverted ? -1 : 1;
  const startX = event.clientX;
  const startWidth = props.modelValue;
  // 本地记住最新值：emit 之后 props.modelValue 要等下一次渲染才更新，
  // 直接在 pointerup 里读它会拿到拖动前的旧值、把宽度写回去。
  let latest = startWidth;

  dragging.value = true;
  // 拖拽期间锁掉文本选择与全局光标。不锁的话指针滑出这条 9px 的热区，
  // 光标就变回箭头/文本，看起来像「拖着拖着断了」，还会顺手选中一片文字。
  const body = document.body;
  const previousUserSelect = body.style.userSelect;
  body.style.userSelect = "none";
  body.classList.add("is-resizing-pane");

  const move = (next: PointerEvent) => {
    latest = clamp(startWidth + direction * (next.clientX - startX));
    emit("update:modelValue", latest);
  };
  const finish = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", finish);
    window.removeEventListener("pointercancel", finish);
    body.style.userSelect = previousUserSelect;
    body.classList.remove("is-resizing-pane");
    dragging.value = false;
    emit("commit", latest);
  };
  // 监听挂在 window 上而不是这个元素上：指针一旦快速划出热区，元素自身就
  // 收不到 pointerup 了，拖拽会卡在「按下」状态。
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", finish);
  window.addEventListener("pointercancel", finish);
}

function reset() {
  apply(props.reset, true);
}

// 键盘要能用，否则 role="separator" 是个只有鼠标能操作的假控件。
function onKeydown(event: KeyboardEvent) {
  const direction = props.inverted ? -1 : 1;
  const step = event.shiftKey ? props.step * 4 : props.step;
  if (event.key === "ArrowLeft") apply(props.modelValue - direction * step, true);
  else if (event.key === "ArrowRight") apply(props.modelValue + direction * step, true);
  else if (event.key === "Home") apply(props.min, true);
  else if (event.key === "End") apply(props.max, true);
  else return;
  event.preventDefault();
}
</script>
