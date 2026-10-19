# G3 live check: same LAN vLLM endpoint as g1-live.ps1, plus session/cron/memory CLI.
# Usage (repo root):
#   powershell -File scripts/g3-live.ps1
# Optional env:
#   $env:DANDELION_BASE_URL="http://192.168.183.25:9911/v1"
#   $env:DANDELION_MODEL="Qwen3.8-Flash-Next"
#   $env:DANDELION_THINKING_INTENSITY="low"

$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not $env:DANDELION_BASE_URL) {
    $env:DANDELION_BASE_URL = "http://192.168.183.25:9911/v1"
}
if (-not $env:DANDELION_MODEL) {
    $env:DANDELION_MODEL = "Qwen3.8-Flash-Next"
}
if (-not $env:DANDELION_THINKING_INTENSITY) {
    $env:DANDELION_THINKING_INTENSITY = "low"
}
$env:DANDELION_PERMISSION_MODE = "auto"

Write-Host "cargo test G3 offline (lib + g3_harness + cli parse)..."
cargo test -p dandelion-core --lib --offline
if ($LASTEXITCODE -ne 0) {
    cargo test -p dandelion-core --lib
}
cargo test -p dandelion-core --test g3_harness --offline -- --test-threads=1
if ($LASTEXITCODE -ne 0) {
    cargo test -p dandelion-core --test g3_harness -- --test-threads=1
}
cargo test -p dandelion-cli --offline
if ($LASTEXITCODE -ne 0) {
    cargo test -p dandelion-cli
}

Write-Host "dln doctor + session/cron/memory CLI..."
cargo run -p dandelion-cli -- doctor
cargo run -p dandelion-cli -- session list
cargo run -p dandelion-cli -- memory show
cargo run -p dandelion-cli -- cron list

Write-Host "acceptance prompts (auto permission)..."
cargo run -p dandelion-cli -- chat --permission-mode auto -q "用 delegate_task 开一个子代理，goal 是只读 README.md 前 20 行并写一句摘要。你自己不要调用 read_file。"
cargo run -p dandelion-cli -- chat --permission-mode auto -q "同一轮里：用 read_file 读 README.md，同时用 delegate_task 让子代理总结 Cargo.toml 的 package 名。不要串行先读完再委托。"
cargo run -p dandelion-cli -- chat --permission-mode auto -q "你是谁？用一句原话回答人格设定"
cargo run -p dandelion-cli -- chat --permission-mode auto -q "用 read_file 读产品 data_dir 下 wiki 里的文件，把正文复述出来"
