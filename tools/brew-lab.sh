#!/usr/bin/env bash
# Build and run the standalone drink-minigame harness
# (`plugins/cafe-sim/examples/brew_lab.rs`).
#
#   tools/brew-lab.sh --game froth
#   tools/brew-lab.sh --drink Latte
#   tools/brew-lab.sh --game trace --script trace.lab --shots out/ --shot-every 10
#
# Why a script rather than a cargo alias: the example has to be built through
# the *workspace* so bevy resolves the same feature set as the engine (the
# engine's other dependencies switch extra bevy features on). `cargo run`
# takes a single package, which would resolve fewer features and rebuild bevy
# into target/ from scratch, evicting the engine's own build.
# docs/BUILD_TIMES.md §4.
#
# The lab's own unit tests run with
#   cargo test --workspace --features cafe-sim/lab
# which links the plugin into the engine and so relinks the engine on the next
# plain `cargo build` (~1 min each way). Building *this example* does not.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --workspace --example brew_lab --features cafe-sim/lab

# `cargo run` would set these; running the binary straight does not, and a
# `dynamic_linking` build has no rpath to the toolchain's libstd or to bevy's
# dylib ("Library not loaded: @rpath/libstd-*.dylib").
export DYLD_FALLBACK_LIBRARY_PATH="$PWD/target/debug/deps:$(rustc --print target-libdir):${DYLD_FALLBACK_LIBRARY_PATH:-}"
exec target/debug/examples/brew_lab "$@"
