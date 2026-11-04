#!/usr/bin/env bash
# Build the browser bundle into pkg/ — the one way to build for the web.
#
#   tools/web.sh            dev:     web-dev profile (thin LTO)      → target/web
#   tools/web.sh profile    + bevy spans for tools/web-trace.mjs    → target/web-profile
#   tools/web.sh release    deploy artifact (fat LTO, cgu=1)        → target/web-release
#
# Each mode has its own target directory because `web-profile` changes bevy's
# feature set and `release` is a different profile; keeping them apart keeps
# every mode's incremental build warm (docs/BUILD_TIMES.md §4). Replaces
# `wasm-pack` locally: same two steps (cargo build + wasm-bindgen) without the
# forced release profile or the wasm-opt pass. Set WASM_OPT=1 to run wasm-opt
# on a release build. CI still uses wasm-pack (.github/workflows).
#
# Needs `wasm-bindgen` at the version pinned in Cargo.lock:
#   cargo install wasm-bindgen-cli --version <that version>
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-dev}
features=""
case "$mode" in
  dev)     profile=web-dev; dir=target/web ;;
  profile) profile=web-dev; dir=target/web-profile; features="--features web-profile" ;;
  release) profile=release; dir=target/web-release ;;
  *) echo "usage: tools/web.sh [dev|profile|release]" >&2; exit 2 ;;
esac

want=$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/[^0-9.]/,"",$3); print $3}' Cargo.lock)
if ! command -v wasm-bindgen >/dev/null; then
  echo "wasm-bindgen not installed: cargo install wasm-bindgen-cli --version $want" >&2; exit 1
fi
have=$(wasm-bindgen --version | awk '{print $2}')
if [[ "$have" != "$want" ]]; then
  echo "wasm-bindgen $have installed but Cargo.lock pins $want: cargo install wasm-bindgen-cli --version $want" >&2; exit 1
fi

# shellcheck disable=SC2086  # $features is intentionally word-split (macOS bash 3 has no empty-array-safe "${a[@]}" under set -u)
cargo build --target wasm32-unknown-unknown --profile "$profile" $features --target-dir "$dir"
wasm="$dir/wasm32-unknown-unknown/$profile/game_engine_lib.wasm"
wasm-bindgen --target web --out-dir pkg --out-name game_engine_lib "$wasm"

if [[ "$mode" == release && "${WASM_OPT:-0}" == 1 ]]; then
  wasm-opt -O --enable-bulk-memory --enable-nontrapping-float-to-int -o pkg/game_engine_lib_bg.wasm pkg/game_engine_lib_bg.wasm
fi
ls -la pkg/game_engine_lib_bg.wasm
