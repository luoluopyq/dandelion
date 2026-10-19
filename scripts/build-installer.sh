#!/usr/bin/env bash
# Build the macOS installer(s) for Dandelion. Must run on macOS.
#
#   bash scripts/build-installer.sh [universal|arm64|x64]
#
# universal (default) -> one .dmg for both Apple silicon (M series) and Intel
# arm64               -> Apple silicon only
# x64                 -> Intel only
#
# Frontend build + `dln` staging happen inside Tauri's beforeBuildCommand
# (scripts/darwin-before-build.sh).
# Output: target/<triple>/release/bundle/dmg/Dandelion_<version>_<arch>.dmg

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-universal}"

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: macOS installers can only be built on macOS (this host: $(uname -s))" >&2
    echo "       run this script on a Mac, or use the CI workflow .github/workflows/release.yml" >&2
    exit 1
fi

for tool in cargo npm rustup; do
    command -v "$tool" >/dev/null 2>&1 || { echo "error: '$tool' not found on PATH" >&2; exit 1; }
done

ensure_target() {
    if ! rustup target list --installed | grep -qx "$1"; then
        echo "==> rustup target add $1"
        rustup target add "$1"
    fi
}

case "$MODE" in
    universal)
        ensure_target aarch64-apple-darwin
        ensure_target x86_64-apple-darwin
        TARGET_ARGS=(--target universal-apple-darwin)
        OUT_DIR="target/universal-apple-darwin/release/bundle/dmg"
        ;;
    arm64)
        ensure_target aarch64-apple-darwin
        TARGET_ARGS=(--target aarch64-apple-darwin)
        OUT_DIR="target/aarch64-apple-darwin/release/bundle/dmg"
        ;;
    x64)
        ensure_target x86_64-apple-darwin
        TARGET_ARGS=(--target x86_64-apple-darwin)
        OUT_DIR="target/x86_64-apple-darwin/release/bundle/dmg"
        ;;
    *)
        echo "usage: bash scripts/build-installer.sh [universal|arm64|x64]" >&2
        exit 2
        ;;
esac

echo "==> building Dandelion installer (macos-$MODE)"
npm --prefix "$ROOT/apps/desktop" run tauri build -- "${TARGET_ARGS[@]}" --bundles app,dmg --config src-tauri/tauri.bundle.conf.json

echo
echo "==> artifacts"
find "$ROOT/$OUT_DIR" -maxdepth 1 -type f -name "*.dmg" -exec ls -lh {} \; 2>/dev/null || true
