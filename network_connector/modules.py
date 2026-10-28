"""Wasm modules: hash, size and import checks, and the helper's module cache.

A module is only data here. Nothing in this file compiles or runs it: the import check is a small,
bounds-checked reader of the module's import section, so the helper can refuse a module before it
reaches the sandbox. Full validation and compilation happen inside the sandbox (sandbox_runner.py).
"""
from __future__ import annotations

import hashlib
import os
import re
import secrets
from pathlib import Path

MAX_MODULE_BYTES = 10 * 1024 * 1024
CACHE_BYTES = 200 * 1024 * 1024
WASI = "wasi_snapshot_preview1"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")

# WASI preview 1 calls a module may import. No path_* (files), no sock_* (sockets), no fd_* that touch
# directories. Keep in sync with sandbox_runner.ALLOWED_IMPORTS (tests check they are equal).
ALLOWED_IMPORTS = frozenset({
    "fd_read", "fd_write", "fd_close", "fd_fdstat_get", "fd_seek", "fd_prestat_get", "fd_prestat_dir_name",
    "args_get", "args_sizes_get", "environ_get", "environ_sizes_get", "clock_time_get", "clock_res_get",
    "random_get", "proc_exit", "sched_yield", "poll_oneoff",
})
_KINDS = {0: "function", 1: "table", 2: "memory", 3: "global", 4: "tag"}


class ModuleError(ValueError):
    """The module is too big, malformed, has the wrong hash, or imports something not allowed."""


def sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _uleb(data: bytes, pos: int, max_bytes: int = 5) -> tuple[int, int]:
    value = shift = 0
    for i in range(max_bytes):
        if pos >= len(data):
            raise ModuleError("module is truncated")
        byte = data[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, pos
        shift += 7
    raise ModuleError("module has a malformed number")


def _name(data: bytes, pos: int, end: int) -> tuple[str, int]:
    size, pos = _uleb(data, pos)
    if pos + size > end:
        raise ModuleError("module has a malformed import name")
    try:
        return data[pos:pos + size].decode("utf-8"), pos + size
    except UnicodeDecodeError as exc:
        raise ModuleError("module has an import name that is not UTF-8") from exc


def imports(data: bytes) -> list[tuple[str, str, str]]:
    """(module, name, kind) for every import. Stops at the first non-function import (always refused)."""
    if len(data) < 8 or data[:4] != b"\0asm":
        raise ModuleError("not a WebAssembly module (wrong magic bytes)")
    if data[4:8] != b"\x01\0\0\0":
        raise ModuleError("only core WebAssembly modules (version 1) are supported, not components")
    found: list[tuple[str, str, str]] = []
    pos, seen_import = 8, False
    while pos < len(data):
        section = data[pos]
        size, pos = _uleb(data, pos + 1)
        end = pos + size
        if end > len(data):
            raise ModuleError("module is truncated")
        if section > 13:
            raise ModuleError(f"module has an unknown section {section}")
        if section == 2:
            if seen_import:
                raise ModuleError("module has two import sections")
            seen_import = True
            count, p = _uleb(data, pos)
            for _ in range(count):
                module, p = _name(data, p, end)
                name, p = _name(data, p, end)
                if p >= end:
                    raise ModuleError("module has a malformed import")
                kind = data[p]
                found.append((module, name, _KINDS.get(kind, f"kind {kind}")))
                if kind != 0:
                    return found
                _, p = _uleb(data, p + 1)
                if p > end:
                    raise ModuleError("module has a malformed import")
        pos = end
    return found


def refused_imports(data: bytes) -> list[str]:
    return [f"{m}.{n}" + ("" if kind == "function" else f" ({kind})")
            for m, n, kind in imports(data) if m != WASI or n not in ALLOWED_IMPORTS or kind != "function"]


def check(data: bytes, expected_sha256: str | None = None) -> str:
    """Size, hash and import allowlist. Returns the SHA-256. Raises ModuleError."""
    if not isinstance(data, (bytes, bytearray)) or not data:
        raise ModuleError("module is empty")
    if len(data) > MAX_MODULE_BYTES:
        raise ModuleError(f"module is larger than {MAX_MODULE_BYTES // 1024 // 1024} MB")
    digest = sha256_hex(bytes(data))
    if expected_sha256 is not None and digest != expected_sha256:
        raise ModuleError("module hash does not match; refusing it")
    refused = refused_imports(bytes(data))
    if refused:
        raise ModuleError("module imports functions the sandbox does not offer: " + ", ".join(refused[:8])
                          + ". Only WASI stdin/stdout/stderr, args, clocks and random are available (no files, no sockets).")
    return digest


def validate_with_wasmtime(data: bytes) -> None:
    """Requester-side extra check of the requester's own module. Never called on a helper."""
    import wasmtime

    try:
        wasmtime.Module.validate(wasmtime.Engine(), bytes(data))
    except wasmtime.WasmtimeError as exc:
        raise ModuleError(f"module is not valid WebAssembly: {str(exc).splitlines()[0][:200]}") from exc


class ModuleCache:
    """Per-user, hash-checked, least-recently-used cache of downloaded modules (files only, never run)."""

    def __init__(self, folder: Path, max_bytes: int = CACHE_BYTES) -> None:
        self.folder, self.max_bytes = folder, max_bytes

    def _path(self, sha256: str) -> Path:
        if not SHA256_RE.match(sha256):
            raise ModuleError("module hash must be 64 lowercase hex characters")
        return self.folder / f"{sha256}.wasm"

    def get(self, sha256: str) -> bytes | None:
        path = self._path(sha256)
        try:
            data = path.read_bytes()
        except OSError:
            return None
        if sha256_hex(data) != sha256:
            path.unlink(missing_ok=True)  # damaged on disk: fetch again
            return None
        try:
            os.utime(path)  # most recently used
        except OSError:
            pass
        return data

    def put(self, sha256: str, data: bytes) -> None:
        check(data, sha256)
        path = self._path(sha256)
        self.folder.mkdir(parents=True, exist_ok=True)
        tmp = path.with_name(f".{secrets.token_hex(4)}.tmp")
        tmp.write_bytes(data)
        os.replace(tmp, path)
        self._evict(keep=path)

    def _evict(self, keep: Path) -> None:
        files = sorted((p for p in self.folder.glob("*.wasm") if p != keep), key=lambda p: p.stat().st_mtime)
        total = sum(p.stat().st_size for p in files) + keep.stat().st_size
        for old in files:
            if total <= self.max_bytes:
                break
            total -= old.stat().st_size
            old.unlink(missing_ok=True)

    def size(self) -> int:
        return sum(p.stat().st_size for p in self.folder.glob("*.wasm")) if self.folder.is_dir() else 0
