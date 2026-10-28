"""Throwaway: native "already escaped Wasm" probe (PLAN §22.1 item 4).

Runs as plain Python inside the AppContainer + Job Object and tries what an escaped module would:
network, reading user files, writing outside its folder, child processes, memory. Prints `PROBE {json}`.
Every write that unexpectedly succeeds is deleted again at once and reported.
"""
from __future__ import annotations

import argparse
import ctypes
import json
import os
import socket
import subprocess
import sys
from ctypes import wintypes


def token_info() -> dict:
    adv = ctypes.WinDLL("advapi32", use_last_error=True)
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    adv.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    adv.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
                                        ctypes.POINTER(wintypes.DWORD)]
    adv.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    k32.IsProcessInJob.argtypes = [wintypes.HANDLE, wintypes.HANDLE, ctypes.POINTER(wintypes.BOOL)]
    tok = wintypes.HANDLE()
    adv.OpenProcessToken(k32.GetCurrentProcess(), 0x0008, ctypes.byref(tok))
    val, n = wintypes.DWORD(), wintypes.DWORD()
    adv.GetTokenInformation(tok, 29, ctypes.byref(val), 4, ctypes.byref(n))  # TokenIsAppContainer
    buf = ctypes.create_string_buffer(64)
    adv.GetTokenInformation(tok, 25, buf, 64, ctypes.byref(n))  # TokenIntegrityLevel
    psid = ctypes.cast(buf, ctypes.POINTER(ctypes.c_void_p))[0]
    s = wintypes.LPWSTR()
    adv.ConvertSidToStringSidW(ctypes.c_void_p(psid), ctypes.byref(s))
    in_job = wintypes.BOOL()
    k32.IsProcessInJob(k32.GetCurrentProcess(), None, ctypes.byref(in_job))
    return {"is_appcontainer": bool(val.value), "integrity": s.value, "in_job": bool(in_job.value)}


def attempt(fn) -> str:
    try:
        r = fn()
        return "OK" + (f" {r}" if r is not None else "")
    except BaseException as exc:  # noqa: BLE001 - report everything
        return f"FAIL {type(exc).__name__}: {str(exc)[:120]}"


def connect(host: str, port: int) -> None:
    with socket.create_connection((host, port), timeout=3):
        pass


def udp(host: str, port: int) -> str:
    """Real DNS query (A example.com); only a reply proves the datagram left the box."""
    q = b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01"
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(3)
        s.sendto(q, (host, port))
        try:
            data, _ = s.recvfrom(512)
        except TimeoutError:
            return "sendto returned OK but NO reply (dropped)"
        return f"REPLY {len(data)} bytes"


def udp_n(host: str, port: int) -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        for _ in range(5):
            s.sendto(b"nc-spike-udp", (host, port))
    return "(5 sent)"


def accept(port: int, seconds: float) -> str:
    with socket.socket() as s:
        s.bind(("0.0.0.0", port))
        s.listen(4)
        s.settimeout(seconds)
        try:
            c, peer = s.accept()
        except TimeoutError:
            return "no inbound connection arrived"
        c.close()
        return f"ACCEPTED inbound from {peer}"


def udp_recv(port: int, seconds: float) -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("0.0.0.0", port))
        s.settimeout(seconds)
        try:
            data, peer = s.recvfrom(100)
        except TimeoutError:
            return "no datagram arrived"
        return f"RECEIVED {data!r} from {peer}"


def listen(host: str) -> int:
    s = socket.socket()
    s.bind((host, 0))
    s.listen(1)
    port = s.getsockname()[1]
    s.close()
    return port


def read(path: str) -> int:
    if os.path.isdir(path):
        return len(os.listdir(path))
    with open(path, "rb") as f:
        return len(f.read(4096))


def write(path: str) -> str:
    with open(path, "xb") as f:
        f.write(b"nc-spike")
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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--connect", action="append", default=[], help="host:port")
    ap.add_argument("--read", action="append", default=[])
    ap.add_argument("--write", action="append", default=[])
    ap.add_argument("--child", action="store_true")
    ap.add_argument("--udp", action="append", default=[], help="host:port, send 5 datagrams")
    ap.add_argument("--accept", type=int, default=0, help="TCP port to listen on 0.0.0.0 for 3 s")
    ap.add_argument("--udp-recv", type=int, default=0, help="UDP port to receive on 0.0.0.0 for 3 s")
    ap.add_argument("--alloc-mb", type=int, default=0)
    a = ap.parse_args()
    out: dict = {"token": attempt(token_info), "cwd": os.getcwd(), "env_keys": sorted(os.environ)}
    for hp in a.connect:
        host, port = hp.rsplit(":", 1)
        out[f"tcp {hp}"] = attempt(lambda: connect(host, int(port)))
    if a.connect:
        out["udp dns query 8.8.8.8:53"] = attempt(lambda: udp("8.8.8.8", 53))
        out["dns example.com"] = attempt(lambda: socket.getaddrinfo("example.com", 443)[0][4][0])
        out["listen 0.0.0.0"] = attempt(lambda: listen("0.0.0.0"))
    for hp in a.udp:
        host, port = hp.rsplit(":", 1)
        out[f"udp send {hp}"] = attempt(lambda: udp_n(host, int(port)))
    if a.accept:
        out[f"tcp accept 0.0.0.0:{a.accept}"] = attempt(lambda: accept(a.accept, 3))
    if a.udp_recv:
        out[f"udp recv 0.0.0.0:{a.udp_recv}"] = attempt(lambda: udp_recv(a.udp_recv, 3))
    for p in a.read:
        out[f"read {p}"] = attempt(lambda: read(p))
    for p in a.write:
        out[f"write {p}"] = attempt(lambda: write(p))
    if a.child:
        out["child python"] = attempt(lambda: subprocess.run([sys.executable, "-c", "pass"], timeout=10).returncode)
        out["child cmd"] = attempt(lambda: subprocess.run(["cmd.exe", "/c", "exit 0"], timeout=10).returncode)
    if a.alloc_mb:
        out[f"alloc {a.alloc_mb} MB"] = attempt(lambda: alloc(a.alloc_mb))
    print("PROBE " + json.dumps(out), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
