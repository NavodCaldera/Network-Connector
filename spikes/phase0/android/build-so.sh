#!/usr/bin/env bash
# Phase 0.9a: cross-compile the spike .so for each Wasm runtime, record size and build time,
# and copy each into the matching app flavor. Run from Git Bash on Windows (or any bash).
#
# Needs: rustup target aarch64-linux-android, cargo-ndk, ANDROID_NDK_HOME (or ANDROID_HOME/ndk/<ver>).
# Each variant is a clean build (its own target dir) so build times are comparable.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
crate="$here/wasm-spike"
base="${SPIKE_TARGET_BASE:-${CARGO_TARGET_DIR:-$here/target}}"
jobs="${SPIKE_JOBS:-2}"

for v in pulley cranelift wasmi; do
  td="$base/so-$v"
  rm -rf "$td"
  start=$(date +%s)
  (cd "$crate" && CARGO_TARGET_DIR="$td" cargo ndk -t arm64-v8a --platform 26 build --release -j "$jobs" \
     --no-default-features --features "rt-$v")
  secs=$(( $(date +%s) - start ))
  so="$td/aarch64-linux-android/release/libnc_wasm_spike.so"
  bytes=$(wc -c < "$so")
  echo "SIZE variant=$v bytes=$bytes build_s=$secs"
  dest="$here/app-spike/app/src/$v/jniLibs/arm64-v8a"
  mkdir -p "$dest"
  cp "$so" "$dest/"
done
