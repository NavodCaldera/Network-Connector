"""The Wasm sandbox escape suite (PLAN §22.1) and per-job create/delete (§17.3).

Every case must fail safely: killed or refused, with nothing written outside the sandbox and no profile left.
These run real AppContainers and Job Objects (Windows only).
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from network_connector import modules, sandbox, sandbox_runner
from network_connector.config import detect_lan_ip
from wasm_modules import (ECHO, EXIT_7, HOST_SYSTEM, HUGE_ERR, HUGE_OUT, LOOK_AROUND, LOOP, MEMBOMB, NO_START,
                          PATH_OPEN, READDIR, RECURSE, SLEEP, SOCK_ACCEPT, SOCK_OPEN, wasm)

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="the Wasm sandbox is Windows-only")


def _mine() -> list[str]:
    """Profiles or folders created by this test process that still exist."""
    return [n for n in sandbox.leftovers() if n.startswith(f"{sandbox.PREFIX.lower()}{os.getpid()}-")]


@pytest.fixture
def box(sandbox_runtime):
    yield sandbox_runtime
    assert _mine() == [], "a sandbox profile or folder was left behind"


# ------------------------------------------------------------------ the allowlists agree
def test_runner_and_helper_share_one_import_allowlist():
    assert sandbox_runner.ALLOWED_IMPORTS == modules.ALLOWED_IMPORTS
    assert not any(n.startswith(("path_", "sock_")) for n in modules.ALLOWED_IMPORTS)


# ------------------------------------------------------------------ normal runs
def test_echo_runs_and_profile_is_deleted(box):
    r = sandbox.run(wasm(ECHO), b"hello sandbox", memory_mb=16, seconds=10)
    assert r.outcome == "ok" and r.exit_code == 0 and r.stdout == b"hello sandbox"
    assert r.peak_mem_mb > 0 and r.cpu_seconds >= 0 and not r.escape_signal
    assert r.profile.lower() not in sandbox.leftovers()


def test_exit_code_and_args_are_passed(box):
    r = sandbox.run(wasm(EXIT_7), memory_mb=16, seconds=10)
    assert r.outcome == "exit" and r.exit_code == 7 and r.ok
    r = sandbox.run(wasm(LOOK_AROUND), args=["a", "b"], memory_mb=16, seconds=10)
    # Nothing pre-opened (EBADF = 8), no environment variables, argv = module name + 2 args.
    assert r.stdout == b"P=08 E=00 A=03 "


def test_big_input_reaches_stdin_intact(box):
    data = os.urandom(3 * 1024 * 1024)
    r = sandbox.run(wasm(ECHO), data, memory_mb=16, seconds=20, stdout_cap=4 * 1024 * 1024)
    assert r.outcome == "ok" and r.stdout == data


# ------------------------------------------------------------------ 22.1 item 1: files, env, sockets, host imports
@pytest.mark.parametrize("text, name", [(PATH_OPEN, "path_open"), (READDIR, "fd_readdir"),
                                        (SOCK_ACCEPT, "sock_accept"), (SOCK_OPEN, "sock_open"),
                                        (HOST_SYSTEM, "env.system")])
def test_file_socket_and_host_imports_are_refused_by_both_layers(box, text, name):
    module = wasm(text)
    with pytest.raises(modules.ModuleError, match=name.split(".")[-1]):
        modules.check(module)  # helper side, before anything starts
    r = sandbox.run(module, memory_mb=16, seconds=10)  # and inside the sandbox, before linking
    assert r.outcome == "refused" and name.split(".")[-1] in r.detail and r.stdout == b""


def test_module_without_start_is_refused(box):
    r = sandbox.run(wasm(NO_START), memory_mb=16, seconds=10)
    assert r.outcome == "invalid" and "_start" in r.detail


# ------------------------------------------------------------------ 22.1 item 2: memory, time, stack, output
def test_memory_bomb_stops_at_the_store_limit(box):
    r = sandbox.run(wasm(MEMBOMB), memory_mb=64, seconds=20)
    assert r.outcome == "exit" and r.exit_code == 42  # memory.grow was refused at 64 MB
    assert r.peak_mem_mb < 64 + sandbox.RUNTIME_OVERHEAD_MB


def test_infinite_loop_is_killed_by_the_wall_clock(box):
    t = time.monotonic()
    r = sandbox.run(wasm(LOOP), memory_mb=16, seconds=1)
    assert r.outcome == "timeout"
    assert time.monotonic() - t < 4


def test_sleeping_module_is_killed_by_the_wall_clock(box):
    t = time.monotonic()
    r = sandbox.run(wasm(SLEEP), memory_mb=16, seconds=1)
    assert r.outcome == "timeout" and time.monotonic() - t < 4


def test_one_shot_epoch_gives_a_clean_time_limit(box):
    r = sandbox.run(wasm(LOOP), memory_mb=16, seconds=1, clean_timeout=True)
    assert r.outcome == "timeout"


def test_fuel_limit_traps_and_reports_fuel(box):
    r = sandbox.run(wasm(LOOP), memory_mb=16, seconds=20, fuel=50_000_000)
    assert r.outcome == "trap" and r.detail == "OUT_OF_FUEL"
    r = sandbox.run(wasm(ECHO), b"x", memory_mb=16, seconds=20, fuel=10_000_000)
    assert r.outcome == "ok" and 0 < r.fuel_used < 10_000_000


def test_deep_recursion_hits_the_stack_limit(box):
    r = sandbox.run(wasm(RECURSE), memory_mb=16, seconds=20)
    assert r.outcome == "trap" and r.detail == "STACK_OVERFLOW"


def test_huge_stdout_is_cut_off(box):
    r = sandbox.run(wasm(HUGE_OUT), memory_mb=16, seconds=20)
    assert r.outcome == "stdout_cap" and len(r.stdout) <= sandbox.STDOUT_CAP


def test_huge_stderr_is_capped(box):
    r = sandbox.run(wasm(HUGE_ERR), memory_mb=16, seconds=1)
    assert r.outcome == "timeout" and len(r.stderr) <= sandbox.STDERR_CAP


# ------------------------------------------------------------------ 22.1 item 3: bad modules
def test_invalid_too_big_and_wrong_hash_modules_are_refused(box, tmp_path):
    garbage = b"\0asm\1\0\0\0" + b"\x01\xff\xff\xff\xff\x0f"
    with pytest.raises(modules.ModuleError):
        modules.check(garbage)
    with pytest.raises(modules.ModuleError, match="not a WebAssembly"):
        modules.check(b"MZ\x90\x00 not wasm")
    with pytest.raises(modules.ModuleError, match="larger than"):
        modules.check(wasm(ECHO) + b"\0" * modules.MAX_MODULE_BYTES)
    with pytest.raises(modules.ModuleError, match="hash"):
        modules.check(wasm(ECHO), "0" * 64)
    cache = modules.ModuleCache(tmp_path / "cache")
    with pytest.raises(modules.ModuleError, match="hash"):
        cache.put("0" * 64, wasm(ECHO))
    r = sandbox.run(b"\0asm\1\0\0\0\x0a\x01", memory_mb=16, seconds=10)
    assert r.outcome == "invalid"


# ------------------------------------------------------------------ 22.1 item 4: the native "already escaped" layer
def _probe(*args: str, memory_mb: int = 64, seconds: float = 30) -> tuple[sandbox.SandboxResult, dict]:
    home = sandbox.runtime_home()
    cmd = [str(sandbox.base_python()), "-I", "-S", "-B", str(home / "escape_probe.py"), *args]
    r = sandbox.Sandbox(b"", command=cmd, memory_mb=memory_mb, seconds=seconds).run()
    line = next((x for x in r.stdout.decode(errors="replace").splitlines() if x.startswith("PROBE ")), None)
    return r, json.loads(line[6:]) if line else {}


class _Listener:
    def __init__(self, host: str) -> None:
        self.sock = socket.socket()
        self.sock.bind((host, 0))
        self.sock.listen(8)
        self.sock.settimeout(0.2)
        self.port, self.accepted, self._stop = self.sock.getsockname()[1], 0, threading.Event()
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self.sock.accept()
                self.accepted += 1
                conn.close()
            except OSError:
                pass

    def close(self) -> None:
        self._stop.set()
        self.thread.join(2)
        self.sock.close()


def test_escaped_code_cannot_reach_the_network(box):
    listeners = [_Listener("127.0.0.1")]
    lan = detect_lan_ip()
    if lan != "127.0.0.1":
        listeners.append(_Listener(lan))
    try:
        targets = [f"{l.sock.getsockname()[0]}:{l.port}" for l in listeners] + ["8.8.8.8:53", "1.1.1.1:443"]
        args = [a for t in targets for a in ("--connect", t)] + ["--dns", "example.com"]
        r, probe = _probe(*args)
        assert probe["token"] == {"appcontainer": True, "in_job": True}
        for t in targets:
            assert probe[f"connect {t}"].startswith("FAIL"), (t, probe[f"connect {t}"])
        assert probe["dns example.com"].startswith("FAIL")
        assert all(l.accepted == 0 for l in listeners)  # nothing arrived at the requester's own listeners
    finally:
        for l in listeners:
            l.close()


def test_escaped_code_cannot_read_user_files_or_write_outside(box, tmp_path):
    secret = Path(os.environ["NC_HOME"]) / "controller" / "admin.token"
    secret.parent.mkdir(parents=True, exist_ok=True)
    secret.write_text("canary-not-a-real-token", encoding="utf-8")
    profile = os.environ["USERPROFILE"]
    outside = [str(tmp_path / "canary.tmp"), os.path.join(profile, "nc-escape-canary.tmp"),
               str(Path(os.environ["NC_HOME"]) / "canary.tmp"), str(sandbox.runtime_home() / "canary.tmp")]
    args = ["--read", profile, "--read", str(secret), "--read", str(Path(__file__).resolve()),
            "--read", os.environ["NC_HOME"]]
    for path in outside:
        args += ["--write", path]
    args += ["--write", "own-folder.tmp"]
    r, probe = _probe(*args)
    assert probe, r.detail
    for key in ("read " + profile, "read " + str(secret), "read " + str(Path(__file__).resolve()),
                "read " + os.environ["NC_HOME"]):
        assert probe[key].startswith("FAIL"), key
    for path in outside:
        assert probe["write " + path].startswith("FAIL"), path
        assert not Path(path).exists()
    assert probe["write own-folder.tmp"].startswith("OK")  # its own AppContainer folder, deleted afterwards


def test_escaped_code_cannot_start_a_child_process(box):
    r, probe = _probe("--child", "1")
    # Either the child-process policy refused it inside the probe, or the job's limit message got it killed.
    if probe:
        assert probe["child"].startswith("FAIL") and probe["child cmd"].startswith("FAIL")
    else:
        assert r.outcome == "escape"


def test_escaped_code_cannot_exceed_the_memory_cap(box):
    r, probe = _probe("--alloc-mb", "2048", memory_mb=32)
    assert r.outcome == "memory" or "MemoryError" in probe.get("alloc", "")
    assert r.peak_mem_mb <= 32 + sandbox.RUNTIME_OVERHEAD_MB + 1


# ------------------------------------------------------------------ 22.1 item 5: Stop
def test_stop_all_kills_every_sandbox_within_one_second(box):
    boxes = [sandbox.Sandbox(wasm(LOOP), memory_mb=16, seconds=600) for _ in range(2)]
    results: dict[int, sandbox.SandboxResult] = {}
    threads = [threading.Thread(target=lambda i=i, b=b: results.__setitem__(i, b.run())) for i, b in enumerate(boxes)]
    for t in threads:
        t.start()
    deadline = time.monotonic() + 20
    while not all(b._running_at for b in boxes) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert all(b._running_at for b in boxes)
    t0 = time.monotonic()
    assert sandbox.stop_all() == 2
    for t in threads:
        t.join(5)
    assert time.monotonic() - t0 < 1.0
    assert [results[i].outcome for i in range(2)] == ["stopped", "stopped"]
    assert sandbox.active() == [] and sandbox.stop_all() == 0


# ------------------------------------------------------------------ 22.1 item 6: helper crash + start-up sweep
CRASH_HELPER = """
import sys, threading, time
from network_connector import sandbox
box = sandbox.Sandbox(bytes.fromhex(sys.argv[1]), memory_mb=16, seconds=600)
threading.Thread(target=box.run, daemon=True).start()
while not box._running_at:
    time.sleep(0.02)
print(box.name, box.pid, flush=True)
time.sleep(600)
"""


def _alive(pid: int) -> bool:
    import psutil

    try:
        return psutil.Process(pid).status() != psutil.STATUS_ZOMBIE
    except psutil.Error:
        return False


def test_helper_killed_mid_job_then_sweep_removes_the_leftovers(box):
    helper = subprocess.Popen([sys.executable, "-c", CRASH_HELPER, wasm(LOOP).hex()], stdout=subprocess.PIPE,
                              text=True, env=os.environ.copy())
    try:
        name, pid = helper.stdout.readline().split()
        assert _alive(int(pid))
        assert name.lower() in sandbox.leftovers()
        assert sandbox.sweep() == 0 or name.lower() in sandbox.leftovers()  # a live helper's sandbox is kept
        helper.kill()
        helper.wait(10)
        deadline = time.monotonic() + 5
        while _alive(int(pid)) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not _alive(int(pid))  # kill-on-close took the sandbox process with the helper
        assert name.lower() in sandbox.leftovers()  # but the profile is still there
        assert sandbox.sweep() >= 1
        assert name.lower() not in sandbox.leftovers()
        assert not (Path(os.environ["LOCALAPPDATA"]) / "Packages" / name.lower()).exists()
    finally:
        helper.kill()


# ------------------------------------------------------------------ 17.3: 0 leftovers after 100 jobs
@pytest.mark.slow
def test_no_leftovers_after_100_jobs_with_mixed_endings(box):
    before = sandbox.leftovers()
    endings = [
        ("ok", lambda: sandbox.run(wasm(ECHO), b"x", memory_mb=16, seconds=10)),
        ("timeout", lambda: sandbox.run(wasm(LOOP), memory_mb=16, seconds=0.3)),
        ("exit", lambda: sandbox.run(wasm(MEMBOMB), memory_mb=32, seconds=10)),
        ("trap", lambda: sandbox.run(wasm(RECURSE), memory_mb=16, seconds=10)),
        ("stdout_cap", lambda: sandbox.run(wasm(HUGE_OUT), memory_mb=16, seconds=10, stdout_cap=65536)),
        ("refused", lambda: sandbox.run(wasm(PATH_OPEN), memory_mb=16, seconds=10)),
        ("memory", lambda: _probe("--alloc-mb", "1024", memory_mb=16)[0]),
    ]
    seen: dict[str, int] = {}
    for i in range(100):
        expected, fn = endings[i % len(endings)]
        r = fn()
        assert r.outcome == expected, (i, r.outcome, r.detail)
        seen[r.outcome] = seen.get(r.outcome, 0) + 1
        assert r.profile.lower() not in sandbox.leftovers()
    assert sum(seen.values()) == 100
    assert sandbox.leftovers() == before
    assert _mine() == []


# ------------------------------------------------------------------ availability
def test_available_explains_what_is_missing(sandbox_runtime, tmp_path, monkeypatch):
    monkeypatch.setenv("NC_RUNTIME_HOME", str(tmp_path / "not-set-up"))
    ok, reason = sandbox.available(refresh=True)
    assert not ok and "nc sandbox setup" in reason
    monkeypatch.setenv("NC_RUNTIME_HOME", str(Path(os.environ["NC_HOME"]) / "runtime"))
    ok, reason = sandbox.available(refresh=True)
    assert not ok and "secrets" in reason
    with pytest.raises(sandbox.SandboxUnavailable):
        sandbox.setup_runtime()  # never inside the data folder
    assert not (Path(os.environ["NC_HOME"]) / "runtime").exists()
    monkeypatch.undo()
    assert sandbox.available(refresh=True) == (True, "")


def test_unavailable_sandbox_refuses_to_run(sandbox_runtime, tmp_path, monkeypatch):
    monkeypatch.setenv("NC_RUNTIME_HOME", str(tmp_path / "missing"))
    sandbox.available(refresh=True)
    with pytest.raises(sandbox.SandboxUnavailable):
        sandbox.run(wasm(ECHO), b"x")
    monkeypatch.undo()
    sandbox.available(refresh=True)


def test_runtime_folder_has_the_one_grant_and_data_folder_has_none(sandbox_runtime):
    assert sandbox.has_grant(sandbox_runtime, inherit=True)
    assert sandbox.has_grant(sandbox_runtime / sandbox.RUNNER_NAME)
    assert not sandbox.has_grant(Path(os.environ["NC_HOME"]))
    assert sandbox.has_grant(sandbox.base_python())
