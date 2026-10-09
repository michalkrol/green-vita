#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export VITASDK="${VITASDK:-$HOME/vitasdk}"
export PATH="$VITASDK/bin:$PATH"

APP="$ROOT/green-vita"
BRANCH="0.1.9"

if [ ! -x "$VITASDK/bin/arm-vita-eabi-gcc" ]; then
  echo "VitaSDK not found at $VITASDK - run scripts/setup-toolchain.sh first." >&2
  exit 1
fi

if [ ! -d "$APP" ]; then
  echo "==> Cloning green-vita branch $BRANCH (carries the local-streaming fix; master does NOT)"
  git clone https://github.com/Day-OS/green-vita -b "$BRANCH" "$APP"
fi

# Generate unique build identifier: branch-commit-datetime
BUILD_BRANCH=$(cd "$APP" && git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "unknown")
BUILD_COMMIT=$(cd "$APP" && git rev-parse --short HEAD 2>/dev/null || echo "unknown")
BUILD_DATETIME=$(date +"%Y%m%d-%H%M%S")
BUILD_ID="${BUILD_BRANCH}-${BUILD_COMMIT}-${BUILD_DATETIME}"

ARTIFACT_DIR="$APP/target/armv7-sony-vita-newlibeabihf/release"

cmd="${1:-build}"
case "$cmd" in
  build|vpk)
    cd "$APP"
    make vpk
    echo
    echo "VPK ready: $ARTIFACT_DIR/green-vita.vpk"
    # Copy with unique build ID for preservation
    cp "$ARTIFACT_DIR/green-vita.vpk" "$ARTIFACT_DIR/green-vita-${BUILD_ID}.vpk"
    cp "$ARTIFACT_DIR/green-vita.self" "$ARTIFACT_DIR/green-vita-${BUILD_ID}.self" 2>/dev/null || true
    cp "$ARTIFACT_DIR/green-vita.velf" "$ARTIFACT_DIR/green-vita-${BUILD_ID}.velf" 2>/dev/null || true
    cp "$ARTIFACT_DIR/green-vita.elf" "$ARTIFACT_DIR/green-vita-${BUILD_ID}.elf" 2>/dev/null || true
    echo "Preserved as: green-vita-${BUILD_ID}.vpk"
    ;;
  upload-vpk)
    ip="${2:?usage: build.sh upload-vpk <vita-ip>}"
    cd "$APP"
    make upload-vpk VITA_IP="$ip"
    echo "Installed. Launch GreenVita on the Vita (enable Unsafe Homebrew in HENkaku settings)."
    ;;
  run)
    ip="${2:?usage: build.sh run <vita-ip>}"
    cd "$APP"
    make update-run-vita VITA_IP="$ip"
    ;;
  test)
    cd "$APP"
    cargo +nightly test
    ;;
  *)
    echo "usage: build.sh [build|upload-vpk <vita-ip>|run <vita-ip>|test]" >&2
    exit 1
    ;;
esac
