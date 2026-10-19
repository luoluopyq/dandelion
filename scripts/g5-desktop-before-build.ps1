# Build frontend + copy optional `dln.exe` extra for the NSIS installer.
# Invoked from apps/desktop/src-tauri/tauri.conf.json beforeBuildCommand (cwd = repo root).
# `dln.exe` is the same product CLI, not an Agent sidecar.

$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $Root

Write-Host "g5-desktop-before-build: cargo build -p dandelion-cli --release"
cargo build -p dandelion-cli --release
if ($LASTEXITCODE -ne 0) {
    throw "cargo build -p dandelion-cli --release failed"
}

$src = Join-Path $Root "target\release\dln.exe"
if (-not (Test-Path $src)) {
    throw "missing $src"
}
$destDir = Join-Path $Root "apps\desktop\src-tauri\resources"
New-Item -ItemType Directory -Force -Path $destDir | Out-Null
Copy-Item $src (Join-Path $destDir "dln.exe") -Force
# macOS-only extra, keep Windows bundles clean
Remove-Item (Join-Path $destDir "dln") -Force -ErrorAction SilentlyContinue
Write-Host "copied dln.exe extra into $destDir (not an Agent sidecar)"

Write-Host "g5-desktop-before-build: npm run build (apps/desktop)"
npm --prefix (Join-Path $Root "apps\desktop") run build
if ($LASTEXITCODE -ne 0) {
    throw "apps/desktop npm run build failed"
}
