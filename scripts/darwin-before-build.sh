#!/usr/bin/env bash
# macOS counterpart of scripts/g5-desktop-before-build.ps1.
#
# Invoked from tauri.macos.conf.json -> build.beforeBuildCommand (cwd = repo root).
# Tauri exports TAURI_ENV_TARGET_TRIPLE (aarch64-apple-darwin, x86_64-apple-darwin
# or universal-apple-darwin); we mirror that target for the optional `dln` CLI
# extra so a universal app bundle also ships a universal CLI.
#
# `dln` is the same product CLI, not an Agent sidecar.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RES_DIR="$ROOT/apps/desktop/src-tauri/resources"
TARGET="${TAURI_ENV_TARGET_TRIPLE:-}"
mkdir -p "$RES_DIR"

# cargo logs on stderr, so command substitution only yields our own echo.
build_cli() {
    cargo build -p dandelion-cli --release --target "$1" >&2
    printf '%s' "$ROOT/target/$1/release/dln"
}

case "$TARGET" in
    universal-apple-darwin)
        echo "darwin-before-build: building universal dln (arm64 + x86_64)"
        ARM_BIN="$(build_cli aarch64-apple-darwin)"
        X64_BIN="$(build_cli x86_64-apple-darwin)"
        lipo -create -output "$RES_DIR/dln" "$ARM_BIN" "$X64_BIN"
        ;;
    "")
        echo "darwin-before-build: TAURI_ENV_TARGET_TRIPLE unset, building for host"
        cargo build -p dandelion-cli --release
        cp "$ROOT/target/release/dln" "$RES_DIR/dln"
        ;;
    *)
        echo "darwin-before-build: building dln for $TARGET"
        cp "$(build_cli "$TARGET")" "$RES_DIR/dln"
        ;;
esac

chmod +x "$RES_DIR/dln"
rm -f "$RES_DIR/dln.exe" # Windows-only extra, keep macOS bundles clean

echo "darwin-before-build: staged $(wc -c <"$RES_DIR/dln") bytes as resources/dln"
echo "darwin-before-build: npm run build (apps/desktop)"
npm --prefix "$ROOT/apps/desktop" run build
