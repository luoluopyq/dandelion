//! 无窗托管回归探针 —— 不是 cargo 集成测试，由
//! agent_plugins::tests::hosted_children_get_no_console_window 在运行时用
//! rustc 编译后调用。
//!
//! 编译产物是 GUI 子系统（PE Subsystem=2）的 exe，与 Tauri 桌面宿主同形：
//! 父进程没有控制台。它再用管道拉起一个控制台子系统子进程（powershell），
//! 按 argv[1] 决定是否设置 CREATE_NO_WINDOW，并把子进程自报的
//! GetConsoleWindow() 原样透传给调用方。
//! 不带 hide 时子进程会被新分配一个带可见窗口的控制台 —— 正是安装版
//! Claude Code 弹出黑色 "claude" 窗口的同一机制。

#![windows_subsystem = "windows"]

fn main() {
    let hide = std::env::args().nth(1).as_deref() == Some("hide");
    let mut child = std::process::Command::new("powershell.exe");
    child
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r#"(Add-Type -MemberDefinition '[System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern System.IntPtr GetConsoleWindow();' -Name K -PassThru)::GetConsoleWindow()"#,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if hide {
        use std::os::windows::process::CommandExt;
        // 与 agent_plugins::hide_console_windows 同一标志值；接线正确性由
        // every_spawn_site_in_agent_plugins_hides_its_console 源码审计测试钉住。
        child.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let line = match child.output() {
        Ok(out) => format!(
            "{}|{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) => format!("spawn-error|{err}"),
    };
    // GUI 子系统下 stdout 仍有效：测试父进程传入了管道。
    let _ = std::io::Write::write_fmt(&mut std::io::stdout(), format_args!("{line}"));
}
