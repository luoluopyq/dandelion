# Build the Windows installer for Dandelion (NSIS, per-user install).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-installer.ps1
#
# Frontend build + `dln.exe` staging happen inside Tauri's beforeBuildCommand
# (scripts/g5-desktop-before-build.ps1), so this script is just the entry point.
# Output: target\release\bundle\nsis\Dandelion_<version>_x64-setup.exe

param(
    [ValidateSet("nsis", "msi", "all")]
    [string]$Bundler = "nsis"
)

$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $Root

foreach ($tool in @("cargo", "npm")) {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "required tool '$tool' not found on PATH"
    }
}

$bundles = if ($Bundler -eq "all") { "nsis,msi" } else { $Bundler }

# Bundler toolchains live in %LOCALAPPDATA%\tauri and are fetched from GitHub on
# first use: NSIS (~2MB) and, for MSI, WiX 3 (~10MB). Both are cached forever
# afterwards. Warn early when the needed one is missing so a download timeout
# is not mistaken for a build failure.
$toolCache = Join-Path $env:LOCALAPPDATA "tauri"
$needNsis = $Bundler -in @("nsis", "all")
$needWix = $Bundler -in @("msi", "all")

if ($needWix -and -not (Test-Path (Join-Path $toolCache "WixTools314"))) {
    Write-Host ""
    Write-Host "==> WiX toolchain not cached: this build must download"
    Write-Host "    https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip"
    Write-Host "    If it times out (common behind a firewall), pick one:"
    Write-Host "      * build NSIS instead (no WiX needed):  scripts\build-installer.cmd"
    Write-Host "      * pre-seed the cache by unzipping that file into"
    Write-Host "        $toolCache\WixTools314"
    Write-Host '      * point Tauri at a GitHub mirror:  $env:TAURI_BUNDLER_TOOLS_GITHUB_MIRROR = "<mirror root>"'
    Write-Host ""
}
if ($needNsis -and -not (Test-Path (Join-Path $toolCache "NSIS"))) {
    Write-Host "==> NSIS toolchain not cached: this build must download it from GitHub (~2MB)"
}

Write-Host "==> building Dandelion installer (windows-x64, bundles: $bundles)"
Write-Host "    first run downloads NSIS into %LOCALAPPDATA%\tauri"
npm --prefix (Join-Path $Root "apps\desktop") run tauri build -- --bundles $bundles --config src-tauri/tauri.bundle.conf.json
if ($LASTEXITCODE -ne 0) {
    throw "tauri build failed with exit code $LASTEXITCODE"
}

Write-Host ""
Write-Host "==> artifacts"
foreach ($dir in @("target\release\bundle\nsis", "target\release\bundle\msi")) {
    $full = Join-Path $Root $dir
    if (Test-Path $full) {
        Get-ChildItem $full -File | Where-Object { $_.Extension -in ".exe", ".msi" } |
            ForEach-Object { Write-Host ("    " + $_.FullName + "  (" + [math]::Round($_.Length / 1MB, 1) + " MB)") }
    }
}
