#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 必须在创建任何窗口之前：强制 PerMonitorV2 DPI 感知。
    // 跨 DPI 显示器拖拽时，如果进程没声明 PerMonitorV2，Windows 会把新 DPI 下的物理像素
    // 当成逻辑像素交给 WebView2，窗口瞬间放大数倍。这里兜底覆盖 tauri-build 默认 manifest。
    #[cfg(windows)]
    dandelion_desktop_lib::enable_per_monitor_v2_dpi();

    dandelion_desktop_lib::run();
}
