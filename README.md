# Dandelion（蒲公英）

Dandelion 是本地优先的 AI Agent 桌面应用，同时提供命令行工具 `dln`。桌面端使用 Tauri 2、Vue 3；桌面端与 CLI 在进程内共用 Rust `dandelion-core` 的 Agent 运行时。模型可连接兼容 OpenAI API 的云端或本地服务。

## 环境要求

- Rust stable（项目的 `rust-toolchain.toml`）
- Node.js 22 和 npm
- Windows：Microsoft C++ Build Tools 和 WebView2 Runtime
- macOS：Xcode Command Line Tools（`xcode-select --install`）

## 启动

在仓库根目录安装前端依赖，然后启动桌面端：

```bash
npm --prefix apps/desktop ci
npm --prefix apps/desktop run tauri dev
```

也可以直接运行 CLI：

```bash
cargo run -p dandelion-cli -- doctor
cargo run -p dandelion-cli -- chat
```

首次对话前，在桌面端配置模型，或为 CLI 设置 `DANDELION_BASE_URL`、`DANDELION_MODEL`；需要鉴权时再设置 `DANDELION_API_KEY`。例如连接本地兼容 OpenAI API 的服务：

```powershell
# Windows PowerShell
$env:DANDELION_BASE_URL = "http://127.0.0.1:11434/v1"
$env:DANDELION_MODEL = "your-model-id"
cargo run -p dandelion-cli -- chat
```

```bash
# macOS shell
export DANDELION_BASE_URL="http://127.0.0.1:11434/v1"
export DANDELION_MODEL="your-model-id"
cargo run -p dandelion-cli -- chat
```

需要独立安装 CLI 时运行 `cargo install --path crates/dandelion-cli --locked`，之后可直接使用 `dln doctor`、`dln chat`。

## Windows 打包

在 Windows 仓库根目录执行：

```powershell
npm --prefix apps/desktop ci
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-installer.ps1
```

默认生成 NSIS 安装器：`target/release/bundle/nsis/Dandelion_<version>_x64-setup.exe`。也可以运行 `scripts/build-installer.cmd`；如需 MSI，给 PowerShell 脚本加 `-Bundler msi`。首次打包时，Tauri 可能下载 NSIS；MSI 另需 WiX。

## macOS 打包

在 Mac 的仓库根目录执行：

```bash
xcode-select --install
npm --prefix apps/desktop ci
bash scripts/build-installer.sh
```

默认生成兼容 Apple silicon 与 Intel 的安装镜像：`target/universal-apple-darwin/release/bundle/dmg/Dandelion_<version>_universal.dmg`。只打单一架构时，使用 `bash scripts/build-installer.sh arm64` 或 `bash scripts/build-installer.sh x64`。脚本会安装缺失的 Rust target。未签名的本地构建首次打开时可能需要在 Finder 中右键选择“打开”。

安装包必须在对应操作系统上构建；仓库的 `.github/workflows/release.yml` 可分别构建 Windows 和 macOS 产物。

## 源码范围与许可

本仓库发布 Dandelion 桌面端、`dandelion-core` 和 `dln` CLI 的源码。运营后台服务源码不在本仓库；桌面正式版需要连接独立部署的账号服务。

本仓库自本次公开版本起采用 [PolyForm Noncommercial License 1.0.0](LICENSE)。允许符合许可条款的非商业使用、修改和分发；商业用途需另行取得许可。第三方依赖及素材遵循各自的许可。本许可不是 OSI 认可的开源许可，因此本项目属于“源码公开”，不应标注为 OSI 开源项目。
