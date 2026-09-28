#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export VITASDK="${VITASDK:-$HOME/vitasdk}"
export PATH="$VITASDK/bin:$PATH"

step() { printf '\n==> %s\n' "$*"; }

step "VitaSDK will live at: $VITASDK"
if [ -e /usr/local/vitasdk ] && [ "$VITASDK" != "/usr/local/vitasdk" ]; then
  echo "NOTE: an existing /usr/local/vitasdk was found; re-run with VITASDK=/usr/local/vitasdk to reuse it."
fi

if ! command -v brew >/dev/null 2>&1; then
  echo "Homebrew is missing (https://brew.sh). Install it, then re-run." >&2
  exit 1
fi

for dep in git cmake pkg-config; do
  if ! command -v "$dep" >/dev/null 2>&1; then
    step "Installing host dependency: $dep"
    brew install "$dep"
  fi
done

mkdir -p "$ROOT/.cache"
if [ ! -d "$ROOT/.cache/vdpm" ]; then
  step "Fetching vdpm (VitaSDK package manager)"
  git clone https://github.com/vitasdk/vdpm "$ROOT/.cache/vdpm"
fi

if [ ! -x "$VITASDK/bin/arm-vita-eabi-gcc" ]; then
  step "Installing VitaSDK core toolchain into $VITASDK"
  cd "$ROOT/.cache/vdpm"
  ./bootstrap-vitasdk.sh
else
  step "VitaSDK core already present at $VITASDK"
fi

step "Building/installing VitaSDK libraries incl. SDL2 + Opus ports"
step "(this is the long part: expect ~15-45 min, safe to re-run)"
cd "$ROOT/.cache/vdpm"
./install-all.sh

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup is missing (https://rustup.rs). Install it, then re-run." >&2
  exit 1
fi

step "Installing Rust nightly + armv7-sony-vita-newlibeabihf target"
rustup toolchain install nightly
rustup target add armv7-sony-vita-newlibeabihf --toolchain nightly

if ! cargo +nightly vita --version >/dev/null 2>&1; then
  step "Installing cargo-vita (compiles from source, a few minutes)"
  cargo +nightly install cargo-vita --locked
fi

cat <<EOF

=======================================================
 Toolchain ready.

 Add this to ~/.zshrc (or ~/.bashrc):

   export VITASDK="$VITASDK"
   export PATH="\$VITASDK/bin:\$PATH"

 Next step:
   bash scripts/build.sh            # clone + build green-vita.vpk
   bash scripts/build.sh upload-vpk <vita-ip>   # first-time install
   bash scripts/build.sh run <vita-ip>          # fast eboot redeploy
=======================================================
EOF
