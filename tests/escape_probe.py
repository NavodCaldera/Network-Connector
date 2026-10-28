"""Native "already escaped Wasm" probe (PLAN §22.1 item 4). Standard library only.

The sandbox tests copy this file into the sandbox runtime folder and start it with the same AppContainer +
Job Object launch as a real job. It tries what escaped code would try and prints `PROBE {json}` on stdout.
For files it reports only success or failure, never contents. A write that unexpectedly succeeds is
deleted again at once.
"""
from __future__ import annotations

import ctypes
import json
import os
import socket
import subprocess
import sys
from ctypes import wintypes


def attempt(fn) -> str:
    try:
        value = fn()
        return "OK" + ("" if value is None else f" {value}")
    except BaseException as exc:  # noqa: BLE001 - report everything
        return f"FAIL {type(exc).__name__}: {str(exc)[:100]}"


def connect(host: str, port: int) -> None:
    with socket.create_connection((host, port), timeout=2):
        pass


def read(path: str) -> int:
    if os.path.isdir(path):
        return len(os.listdir(path))
    with open(path, "rb") as f:
        return len(f.read(16))


def write(path: str) -> str:
    with open(path, "xb") as f:
        f.write(b"nc-escape-canary")
    os.remove(path)
    return "(written and removed)"


def alloc(mb: int) -> str:
    blocks, got = [], 0
    try:
        while got < mb:
            blocks.append(bytearray(16 * 1024 * 1024))
            got += 16
    except MemoryError:
        return f"MemoryError after {got} MB"
    return f"allocated {got} MB"


def token() -> dict:
    adv = ctypes.WinDLL("advapi32", use_last_error=True)
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    adv.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    adv.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
                                        ctypes.POINTER(wintypes.DWORD)]
    k32.IsProcessInJob.argtypes = [wintypes.HANDLE, wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL)]
    tok = wintypes.HANDLE()
    adv.OpenProcessToken(k32.GetCurrentProcess(), 0x0008, ctypes.byref(tok))
    is_ac, n = wintypes.DWORD(), wintypes.DWORD()
    adv.GetTokenInformation(tok, 29, ctypes.byref(is_ac), 4, ctypes.byref(n))  # TokenIsAppContainer
    in_job = wintypes.BOOL()
    k32.IsProcessInJob(k32.GetCurrentProcess(), None, ctypes.byref(in_job))
    return {"appcontainer": bool(is_ac.value), "in_job": bool(in_job.value)}


def main(argv: list[str]) -> int:
    out: dict = {"token": token()}
    i = 0
    while i < len(argv):
        op, arg = argv[i], argv[i + 1] if i + 1 < len(argv) else ""
        i += 2
        if op == "--connect":
            host, port = arg.rsplit(":", 1)
            out[f"connect {arg}"] = attempt(lambda: connect(host, int(port)))
        elif op == "--dns":
            out[f"dns {arg}"] = attempt(lambda: socket.getaddrinfo(arg, 443)[0][4][0])
        elif op == "--read":
            out[f"read {arg}"] = attempt(lambda: read(arg))
        elif op == "--write":
            out[f"write {arg}"] = attempt(lambda: write(arg))
        elif op == "--child":
            out["child"] = attempt(lambda: subprocess.run([sys.executable, "-c", "pass"], timeout=10).returncode)
            out["child cmd"] = attempt(lambda: subprocess.run(["cmd.exe", "/c", "exit 0"], timeout=10).returncode)
        elif op == "--alloc-mb":
            out["alloc"] = attempt(lambda: alloc(int(arg)))
    sys.stdout.write("PROBE " + json.dumps(out) + "\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
