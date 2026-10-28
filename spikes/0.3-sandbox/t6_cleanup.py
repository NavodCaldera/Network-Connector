"""Throwaway: item 6. 50 sandboxed runs with mixed endings leave no profile, registry mapping or folder;
a helper killed mid-job leaves a profile that the start-up sweep removes."""
from __future__ import annotations

import ctypes
import json
import os
import statistics
import subprocess
import time
import winreg
from pathlib import Path

import ac

RUNS = 50
TEMP = Path(os.environ["TEMP"])


def counts() -> dict:
    pkgs = len(list((Path(os.environ["LOCALAPPDATA"]) / "Packages").iterdir()))
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, ac.MAPPINGS) as k:
        maps = winreg.QueryInfoKey(k)[0]
    temp_hits = [p.name for p in TEMP.iterdir() if "sandbox" in p.name.lower() or "nc-spike" in p.name.lower()]
    return {"packages_folders": pkgs, "appcontainer_mappings": maps, "temp_matches": temp_hits,
            "ours": ac.leftovers()}


def alive(pid: int) -> bool:
    h = ac.k32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not h:
        return False
    code = ctypes.c_ulong()
    ac.k32.GetExitCodeProcess(h, ctypes.byref(code))
    ac.k32.CloseHandle(h)
    return code.value == 259  # STILL_ACTIVE


LEAVE_FILES = ("import os; os.makedirs('sub/deeper', exist_ok=True); open('sub/deeper/a.bin','wb').write(b'x'*1048576);"
               "open('b.txt','w').write('left behind'); open(os.path.join(os.environ['TEMP'],'c.tmp'),'w').write('t')")
KINDS = [
    ("ok echo", lambda: ac.run(ac.runner_cmd("echo.wasm", "--fast-platform"), b"hi")),
    ("leaves files in its folder", lambda: ac.run([ac.PY_BASE, "-I", "-S", "-B", "-c", LEAVE_FILES])),
    ("timeout kill", lambda: ac.run(ac.runner_cmd("loop.wasm", "--fast-platform"), timeout=1.0)),
    ("job memory kill", lambda: ac.run(ac.runner_cmd("membomb.wasm", "--fast-platform", "--mem-mb", "1024"),
                                       mem_mb=128)),
    ("crash (abort)", lambda: ac.run([ac.PY_BASE, "-I", "-S", "-B", "-c", "import os; os.abort()"])),
    ("stdout cap exit", lambda: ac.run(ac.runner_cmd("huge_out.wasm", "--fast-platform", "--quiet-stdout"))),
]


def main() -> None:
    print("before:", json.dumps(counts()))
    ac.k32.OpenProcess.restype = ac.wintypes.HANDLE
    ac.k32.OpenProcess.argtypes = [ac.wintypes.DWORD, ac.wintypes.BOOL, ac.wintypes.DWORD]

    c, d = [], []
    for i in range(20):
        name = f"{ac.PREFIX}bare-{i}"
        t = time.perf_counter()
        sid = ac.create_profile(name)
        ac.folder_path(ac.sid_string(sid))
        c.append((time.perf_counter() - t) * 1000)
        ac.adv.FreeSid(sid)
        t = time.perf_counter()
        assert ac.delete_profile(name) == 0
        d.append((time.perf_counter() - t) * 1000)
    print(f"bare profile create+folder lookup: median {statistics.median(c):.1f} ms (max {max(c):.1f}); "
          f"DeleteAppContainerProfile: median {statistics.median(d):.1f} ms (max {max(d):.1f})")

    tally: dict[str, list] = {}
    bad = []
    for i in range(RUNS):
        kind, fn = KINDS[i % len(KINDS)]
        r = fn()
        tally.setdefault(kind, []).append(f"{r.exit_code:#x}")
        if r.leftover_after_delete or r.delete_hr != 0 or Path(r.folder).exists():
            bad.append((kind, r.name, r.delete_hr, r.leftover_after_delete, Path(r.folder).exists()))
    for kind, codes in tally.items():
        print(f"   {kind:28s} x{len(codes)} exit codes {sorted(set(codes))}")
    print(f"{RUNS} runs, runs with leftovers right after delete: {bad}")
    print("after 50 runs:", json.dumps(counts()))

    print("## helper killed mid-job")
    p = subprocess.Popen([ac.PY_BASE, str(ac.HERE / "crash_helper.py")], stdout=subprocess.PIPE, cwd=str(ac.HERE))
    info = json.loads(p.stdout.readline())
    time.sleep(1.0)
    print(f"   sandbox pid {info['pid']} alive before helper kill: {alive(info['pid'])}")
    p.kill()
    p.wait()
    time.sleep(0.5)
    print(f"   sandbox pid alive after helper kill: {alive(info['pid'])} (job kill-on-close)")
    print(f"   leftovers after crash: {ac.leftovers()}")
    print(f"   sweep: {ac.sweep()}")
    print("final:", json.dumps(counts()))


if __name__ == "__main__":
    main()
