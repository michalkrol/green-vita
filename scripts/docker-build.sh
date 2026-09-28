#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/green-vita"
BRANCH="0.1.9"
IMAGE="ghcr.io/vita-rust/vitasdk-rs:latest"

command -v docker >/dev/null 2>&1 || {
  echo "Docker is missing - install Docker Desktop (or OrbStack), start it, then re-run." >&2
  exit 1
}

if [ ! -d "$APP/.git" ]; then
  echo "==> Cloning green-vita branch $BRANCH (carries the local-streaming fix)"
  git clone https://github.com/Day-OS/green-vita -b "$BRANCH" "$APP"
fi

run_in_container() {
  docker run --rm \
    -v "$APP:/work" -w /work \
    -v green-vita-cargo-registry:/usr/local/cargo/registry \
    -v green-vita-cargo-git:/usr/local/cargo/git \
    -v green-vita-vita-stdlib:/usr/local/rustup/toolchains/nightly-x86_64-unknown-linux-musl/lib/rustlib/armv7-sony-vita-newlibeabihf \
    -e RUSTFLAGS="-C target-feature=-neon" \
    "$IMAGE" bash -lc "$*"
}

cmd="${1:-build}"
case "$cmd" in
  build)
    echo "==> Building VPK inside $IMAGE (first run: big image pull + full dep compile, ~20-40 min)"
    echo "==> Rebuilds are fast; crates cache in the green-vita-cargo-* docker volumes."
    run_in_container "cargo vita build vpk --release"
    echo
    echo "VPK ready: $APP/target/armv7-sony-vita-newlibeabihf/release/green-vita.vpk"
    ;;
  test)
    run_in_container "cargo test"
    ;;
  upload-vpk)
    ip="${2:?usage: docker-build.sh upload-vpk <vita-ip>}"
    run_in_container "cargo vita upload --vita-ip '$ip' \
      --source target/armv7-sony-vita-newlibeabihf/release/green-vita.vpk \
      --destination ux0:/data/"
    echo "Installed. Launch GreenVita on the Vita (Unsafe Homebrew must be enabled)."
    ;;
  run)
    ip="${2:?usage: docker-build.sh run <vita-ip>}"
    run_in_container "cargo vita build eboot --update --run --vita-ip '$ip' -- --release"
    ;;
  push)
    ip="${2:?usage: docker-build.sh push <vita-ip>}"
    self="$APP/target/armv7-sony-vita-newlibeabihf/release/green-vita.self"
    [ -f "$self" ] || {
      echo "No green-vita.self yet - run 'docker-build.sh build' first." >&2
      exit 1
    }
    echo "==> Copying prebuilt eboot.bin to ux0:/app/GREENVITA/ (no compile)"
    docker run --rm \
      -v "$APP:/work:ro" -w /work \
      "$IMAGE" bash -lc "cargo vita upload --vita-ip '$ip' \
        --source target/armv7-sony-vita-newlibeabihf/release/green-vita.self \
        --destination ux0:/app/GREENVITA/eboot.bin"
    echo "==> Launching via vitacompanion"
    if ! printf 'launch GREENVITA\n' | nc -w 3 "$ip" 1338; then
      echo "Could not reach command server (is the Vita awake? vitacompanion installed?) -" \
        "launch GreenVita from the home screen manually." >&2
    fi
    ;;
  *)
    echo "usage: docker-build.sh [build|test|upload-vpk <ip>|run <ip>|push <ip>]" >&2
    exit 1
    ;;
esac
