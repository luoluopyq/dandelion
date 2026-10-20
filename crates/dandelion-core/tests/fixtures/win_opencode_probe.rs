//! OpenCode run 适配回归探针 —— 不是 cargo 集成测试，由
//! agent_plugins::tests 中的 Windows 回归测试用 rustc 现场编译后充当
//! npm 布局里的原生 `opencode.exe`。
//!
//! 它模拟官方 `opencode run … -- <消息>` 的参数契约：
//! 1. 把收到的全部 argv（去掉自身路径）以 \u{1f} 连接写入 exe 旁边的
//!    `last_args.txt`，让测试能核验**实际被启动的程序**收到了什么；
//! 2. 找到 `--` 后的位置参数消息，按 OpenCode stream-json 的
//!    `{"type":"text","part":{"text":…}}` 事件把消息**原样**回显到
//!    stdout（消息用 JSON 转义手工编码，避免依赖外部 crate）。
//! 只有真正把消息完整传到 exe 的调用方式才能让 answer 与 prompt 相等，
//! 批处理垫片篡改/拒收参数的场景会直接暴露。

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let _ = std::fs::write(dir.join("last_args.txt"), args.join("\u{1f}"));
        }
    }
    let Some(dash) = args.iter().position(|a| a == "--") else { return };
    let Some(message) = args.get(dash + 1) else { return };
    let mut escaped = String::new();
    for c in message.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    println!("{{\"type\":\"text\",\"sessionID\":\"probe-session\",\"part\":{{\"text\":\"{escaped}\"}}}}");
}
