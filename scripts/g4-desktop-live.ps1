# G4 + CD1 live check against a configured model endpoint.
# Usage (repo root):
#   powershell -File scripts/g4-desktop-live.ps1
# Required env:
#   $env:DANDELION_BASE_URL="http://127.0.0.1:11434/v1"
# Optional env:
#   $env:DANDELION_MODEL="Qwen3.8-Flash-Next"
#   $env:DANDELION_THINKING_INTENSITY="low"
#
# Desktop is in-process Runtime. Do not use `dln serve` as the GUI backend.
# Vue must not call /v1/chat/completions (enforced by dandelion-desktop tests).

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not $env:DANDELION_BASE_URL) {
    throw "Set DANDELION_BASE_URL to the endpoint for this live check."
}
if (-not $env:DANDELION_MODEL) {
    $env:DANDELION_MODEL = "Qwen3.8-Flash-Next"
}
if (-not $env:DANDELION_THINKING_INTENSITY) {
    $env:DANDELION_THINKING_INTENSITY = "low"
}
$env:DANDELION_LIVE_TEST = "1"
$env:DANDELION_PERMISSION_MODE = "auto"

Write-Host "=== offline: core lib + harness + CD1 + CLI + desktop ==="
cargo test -p dandelion-core --lib --offline
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --lib }
if ($LASTEXITCODE -ne 0) { throw "core --lib failed" }

cargo test -p dandelion-core --test g1_harness --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g1_harness -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g1_harness failed" }

cargo test -p dandelion-core --test g2_extensibility --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g2_extensibility -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g2_extensibility failed" }

cargo test -p dandelion-core --test g3_harness --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g3_harness -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g3_harness failed" }

cargo test -p dandelion-core --test cd1_session --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test cd1_session -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "cd1_session failed" }

cargo test -p dandelion-core --test cd2_mcp --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test cd2_mcp -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "cd2_mcp failed" }

cargo test -p dandelion-core --test cd3_sandbox --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test cd3_sandbox -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "cd3_sandbox failed" }

cargo test -p dandelion-core --test g5b_assets --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5b_assets -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5b_assets failed" }

cargo test -p dandelion-core --test g5c_settings --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5c_settings -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5c_settings failed" }

cargo test -p dandelion-core --test g5e_ext --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5e_ext -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5e_ext failed" }

cargo test -p dandelion-core --test g5f_sidecar --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5f_sidecar -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5f_sidecar failed" }

cargo test -p dandelion-core --test g5g_corpus --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5g_corpus -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5g_corpus failed" }

cargo test -p dandelion-core --test g5h_todo --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5h_todo -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5h_todo failed" }

cargo test -p dandelion-core --test g5h_web --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5h_web -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5h_web failed" }

cargo test -p dandelion-core --test g5h_terminal --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-core --test g5h_terminal -- --test-threads=1 }
if ($LASTEXITCODE -ne 0) { throw "g5h_terminal failed" }

cargo test -p dandelion-cli --offline
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-cli }
if ($LASTEXITCODE -ne 0) { throw "dandelion-cli tests failed" }

cargo test -p dandelion-desktop --offline
if ($LASTEXITCODE -ne 0) { cargo test -p dandelion-desktop }
if ($LASTEXITCODE -ne 0) { throw "dandelion-desktop tests failed" }

Write-Host "=== live_g1 (true model) ==="
cargo test -p dandelion-core --test live_g1 -- --nocapture --test-threads=1
if ($LASTEXITCODE -ne 0) { throw "live_g1 failed" }

Write-Host "=== dln doctor + session CLI ==="
cargo run -p dandelion-cli -- doctor
if ($LASTEXITCODE -ne 0) { throw "dln doctor failed" }

Write-Host "=== CLI chat (IX-G4) ==="
cargo run -p dandelion-cli -- chat --permission-mode auto -q "用一个词回答：pong"
if ($LASTEXITCODE -ne 0) { throw "chat ping failed" }
cargo run -p dandelion-cli -- chat --permission-mode auto -q "用 read_file 读 README.md 的前 40 行并总结"
if ($LASTEXITCODE -ne 0) { throw "chat read_file failed" }

$listed = cargo run -p dandelion-cli -- session list
if ($LASTEXITCODE -ne 0) { throw "session list failed" }
Write-Host $listed
$first = ($listed | Select-Object -First 1)
if ($first -match "^(tsk_\S+)") {
    $sid = $Matches[1]
    Write-Host "CD1 live: set-title / prefs on $sid"
    cargo run -p dandelion-cli -- session set-title $sid "cd1-live"
    if ($LASTEXITCODE -ne 0) { throw "set-title failed" }
    cargo run -p dandelion-cli -- session set-prefs $sid --temperature 0.4
    if ($LASTEXITCODE -ne 0) { throw "set-prefs failed" }
    cargo run -p dandelion-cli -- session prefs $sid
    cargo run -p dandelion-cli -- chat -c $sid --permission-mode auto -q "上一轮标题改过了，用一个词回答：ok"
    if ($LASTEXITCODE -ne 0) { throw "resume chat failed" }
}

Write-Host @"

IX-G4 / IX-A / IX-B desktop (manual, same data_dir as dln):
  cd apps/desktop
  npm run tauri -- dev
G4: unconfigured guide if loop_ready=false; stream Markdown; Stop waits this session idle;
ask write-file dialog; restore last SQLite session; Vue never calls the model.
G5-A: three tabs, same id cannot open twice; close tab keeps the task; 02 Skill/MCP bindings.
G5-B: Dock 03/05/06 swaps the sidebar and does NOT close session tabs (click again to collapse).
06: type C:\Windows in 添加路径 — must fail with red text. 05 Stop MCP must not stop the chat.
G5-C: Dock 设置打开 07 + 主区设置 Tab；15 改 temperature 后 17 同一键；17 改 maxTokens 后 15 刷新。
16 关 allowShell 后对话里 terminal 回 Error: shell disabled。门 3 deny write_file 不弹窗且不落盘。
12 测试连接是 GET /models，不是第二套循环。C16 目录隐藏。15 联网分组可见。
G5-D / IX-D: 08「分离为独立窗口」开 09（无 Dock，标题「蒲公英 · 独立窗口」），主窗立刻关该 Tab。
同 ID 不能主窗 Tab 与 09 并存；再点打开会前置已有 09 并给出占用错误。09 内 Stop/发送只走这一路 sessionId。
权限对话框在占用该 session 的那扇窗上弹。关 09 = 关 Tab，任务仍在 01。
G5-E / IX-E: 15 目录进 20 新建作业（expr 可 30s），立即跑后 01 出现 kind=cron；停本次只 cancel 这次 session。
22 改 soul 后新开任务下一轮注入可见。21 不能手建孩子；发给父；停树 = 父 cancel。23 打开 hooks 目录，钩子里不准再调模型。
G5-F / IX-F: 15 模型接入导入 GGUF。无 llama-server → 11a → 改用 Ollama 到 12，会话不丢。
有环境：启动 sidecar 后 Q4 能聊；状态栏显示 127.0.0.1 与 n/max。第三路本地排队中；Stop 出队但不杀 sidecar。
退出应用后无 llama-server 残留。HF 下载按钮禁用（F-MDL-03）。禁止 Vue 直连 sidecar URL。
G5-G / IX-G: Dock 知识库上传 txt，02 勾选后提问能引用片段；不勾选则检索段不出现。system 不含知识库正文。
项目理解→19 保持禁用。禁止把 wiki/会话库打进 corpus。不是 WASM Qdrant。
G5-H / IX-H C11: 模型调 todo 后 01 出现待办卡；下一轮勾选状态仍在。Stop 不丢列表。
G5-H / IX-H C12: 模型调 web_search 后 01 工具卡有结果；连不上或超时回 Error: 循环不崩。15 有联网分组。
G5-H / IX-H C13: terminal 工具卡「在终端打开」出 25；关 Tab / 关窗不杀进程；杀进程树 = Stop（Job Object / killpg）。
G5-I / IX-I: tauri build 出一个 NSIS 包（需 NSIS；无 bundler 则只跑离线 g5i_secrets / g5i_packaging）。安装后 dln.exe 能 doctor，与桌面同一 data_dir。导出会话无 Key。退出无 sidecar。窗口尺寸关掉再开仍在。dln.exe 不是 Agent 后端。
C2b: 打开 preflightEnabled 后长会话会在 complete 前 compact；`dln compact -c <id>` 只摘要前缀。关 compact.enabled 时这两条都拒绝。禁止新 ACP 判别器。
C16: `dln spec ask|plan|auto` 写 `.dandelion/specs/`；Generator/Adapter 写不了业务代码；Diff 是程序算的。桌面禁止 Vue 生成 Spec。

"@
Write-Host "g4-desktop-live.ps1 ok"
