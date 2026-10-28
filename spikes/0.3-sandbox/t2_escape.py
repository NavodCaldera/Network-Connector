"""Throwaway: items 2 + 3. Native "escaped" probe in AppContainer + Job Object vs a plain control process.

Listeners on 127.0.0.1 and the LAN IP prove a failed connect really reached nothing (accept count stays 0).
Only counts/lengths are printed for user files, never contents.
"""
from __future__ import annotations

import json
import os
import socket
import sys
import threading

import ac

LAN_IP = sys.argv[1] if len(sys.argv) > 1 else "10.63.182.8"
GATEWAY = sys.argv[2] if len(sys.argv) > 2 else "10.63.182.218"
HOME = os.environ["USERPROFILE"]
SCRATCH = os.environ.get("NC_SPIKE_SCRATCH", str(ac.HERE / "build"))

accepted: dict[str, int] = {}


def listener(host: str) -> int:
    s = socket.socket()
    s.bind((host, 0))
    s.listen(8)
    accepted[host] = 0

    def loop() -> None:
        while True:
            c, _ = s.accept()
            accepted[host] += 1
            c.close()
    threading.Thread(target=loop, daemon=True).start()
    return s.getsockname()[1]


def show(title: str, r: ac.Run) -> None:
    line = next((x for x in r.stdout.decode(errors="replace").splitlines() if x.startswith("PROBE ")), None)
    print(f"## {title}\n   exit={r.exit_code:#x} timed_out={r.timed_out} job_msgs={sorted(set(r.job_msgs))} "
          f"peak={r.peak_mb:.0f} MB")
    if line is None:
        print("   no PROBE line; stderr tail:", r.stderr.decode(errors="replace")[-400:])
        return
    for k, v in json.loads(line[6:]).items():
        print(f"   {k:60s} {v}")


def main() -> None:
    lo, lan = listener("127.0.0.1"), listener(LAN_IP)
    net = ["--connect", f"127.0.0.1:{lo}", "--connect", f"{LAN_IP}:{lan}", "--connect", f"{GATEWAY}:80",
           "--connect", "8.8.8.8:53", "--connect", "1.1.1.1:443"]
    reads = ["--read", HOME, "--read", os.path.join(HOME, ".gitconfig"), "--read", os.path.join(HOME, ".aws"),
             "--read", os.path.join(os.environ["LOCALAPPDATA"], "NetworkConnector"),
             "--read", str(ac.HERE.parent.parent / "AGENT.md"), "--read", str(ac.HERE.parent.parent / ".venv"),
             "--read", r"C:\Windows\win.ini", "--read", str(ac.HERE / "runner.py")]
    safe_writes = ["--write", "canary-cwd.tmp", "--write", os.path.join(SCRATCH, "nc-spike-canary.tmp")]
    outside_writes = ["--write", str(ac.HERE / "nc-spike-canary.tmp"), "--write", os.path.join(HOME, "nc-spike-canary.tmp"),
                      "--write", r"C:\Users\Public\nc-spike-canary.tmp", "--write", r"E:\nc-spike-canary.tmp",
                      "--write", os.path.join(os.environ["TEMP"], "nc-spike-canary.tmp")]

    r = ac.run(ac.probe_cmd(*net, *reads, *safe_writes), appcontainer=False, job=False, cwd=str(ac.HERE / "build"))
    show("CONTROL: same probe, plain process (no AppContainer, no job)", r)
    print("   listener accepts after control:", dict(accepted))
    for k in accepted:
        accepted[k] = 0

    r = ac.run(ac.probe_cmd(*net, *reads, *safe_writes, *outside_writes, "--child"), mem_mb=256)
    show("SANDBOX: AppContainer (no capabilities) + Job (256 MB, ActiveProcessLimit=1)", r)
    print("   listener accepts after sandbox:", dict(accepted), " profile:", r.name, " leftovers:", r.leftover_after_delete)

    r = ac.run(ac.probe_cmd("--child"), job=False)
    show("AppContainer only, no job: can it start children?", r)
    r = ac.run(ac.probe_cmd("--child"), job=False, child_policy=True)
    show("AppContainer + PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY (restricted), no job", r)

    r = ac.run(ac.probe_cmd("--alloc-mb", "1024"), mem_mb=128, kill_on_memory=False)
    show("Job memory cap 128 MB, allocate 1024 MB, no kill-on-message (allocation fails)", r)
    r = ac.run(ac.probe_cmd("--alloc-mb", "1024"), mem_mb=128, kill_on_memory=True)
    show("Job memory cap 128 MB, allocate 1024 MB, helper kills on PROCESS_MEMORY_LIMIT message", r)
    r = ac.run(ac.runner_cmd("membomb.wasm", "--mem-mb", "1024"), mem_mb=128, kill_on_memory=True)
    print(f"## wasm membomb, Store limit 1024 MB but Job cap 128 MB\n   exit={r.exit_code:#x} "
          f"job_msgs={sorted(set(r.job_msgs))} peak={r.peak_mb:.0f} MB stderr_tail={r.stderr.decode(errors='replace')[-200:]!r}")
    print("leftovers:", ac.leftovers())


if __name__ == "__main__":
    main()
