#!/usr/bin/env bash
# Real-kernel test harness. Call as an ordinary user with an active systemd user session.
set -euo pipefail
if [[ ${1:-} != --inside ]]; then
  exec systemd-run --user --scope -p Delegate=yes bash "$0" --inside
fi
scope="/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)"
mkdir "$scope/supervisor"
printf '%s\n' "$$" > "$scope/supervisor/cgroup.procs"
printf '+cpu +memory +pids\n' > "$scope/cgroup.subtree_control"
# nc-exec discovers this ancestor but never changes its controller configuration.
cargo build -p nc-exec --bin nc-exec-worker --example sandbox_probe -j 2 --locked
cargo test -p nc-exec --test sandbox_linux -j 2 --locked -- --ignored --nocapture --test-threads 1
