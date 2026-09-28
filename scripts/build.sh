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

cmd="${1:-build}"
case "$cmd" in
  build|vpk)
    cd "$APP"
    make vpk
    echo
    echo "VPK ready: $APP/target/armv7-sony-vita-newlibeabihf/release/green-vita.vpk"
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
