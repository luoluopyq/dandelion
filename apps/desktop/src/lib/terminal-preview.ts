// 后台任务栏的输出预览：只取最后一条非空行。
//
// 工作单 2026-10-19 第二版·条目 7：耗时计时器每秒触发一次重渲染，预览函数
// 每次渲染都要跑。旧写法（BackgroundTaskBar.vue 内联）把**全部历史**
// split 成行数组再挑最后一行 —— 终端跑一晚上，每秒就重切一晚上的全文。
// 这里从结尾倒着逐行扫，每轮只碰尾巴和尾部的空白行。放在 lib 里是为了能被
// node --test 直接测到（组件内联函数测不到）。

export function latestOutput(text: string): string {
  let end = text.length; // 行区间 [start, end)，end 不含行尾换行
  while (end > 0) {
    let start = end;
    while (start > 0 && text.charCodeAt(start - 1) !== 10) start -= 1;
    const line = text.slice(start, end).trim();
    if (line) return line;
    end = start > 0 ? start - 1 : 0; // 越过换行符退到上一行；空白行继续向上
  }
  return "暂无输出";
}
