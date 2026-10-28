"""Runs ONE WebAssembly module inside the sandbox process. Standalone on purpose.

sandbox.py copies this file into the runtime folder and starts it with the base interpreter as
`python.exe -I -S -B nc_sandbox_runner.py --site <runtime>\\site`, inside a no-capability AppContainer
and a Job Object. It imports only the standard library and wasmtime, never network_connector.

stdin  : 4-byte little-endian header length, the JSON header, the module bytes, then the job input.
         Everything after the module is the module's WASI stdin.
stdout : the module's stdout, capped. Over the cap the process ends at once (it is single-use).
stderr : protocol lines only: "STAGE <name>" and one final "RESULT {json}". The module's own stderr
         is capped and returned base64-encoded inside RESULT.

WASI gets argv and stdin/stdout/stderr only: no pre-opened folders, no environment, no sockets.
Imports outside ALLOWED_IMPORTS are refused before linking.
"""
from __future__ import annotations

import base64
import json
import os
import struct
import sys
import threading
import time

T_START = time.perf_counter()
MAX_HEADER = 64 * 1024
MAX_MODULE = 10 * 1024 * 1024
STACK_BYTES = 512 * 1024
WASI = "wasi_snapshot_preview1"
# Keep in sync with network_connector.modules.ALLOWED_IMPORTS (a test checks they are equal).
ALLOWED_IMPORTS = frozenset({
    "fd_read", "fd_write", "fd_close", "fd_fdstat_get", "fd_seek", "fd_prestat_get", "fd_prestat_dir_name",
    "args_get", "args_sizes_get", "environ_get", "environ_sizes_get", "clock_time_get", "clock_res_get",
    "random_get", "proc_exit", "sched_yield", "poll_oneoff",
})


def emit(line: str) -> None:
    data = (line + "\n").encode("utf-8", "replace")
    while data:
        data = data[os.write(2, data):]


def result(**fields: object) -> None:
    fields["t_total_ms"] = round((time.perf_counter() - T_START) * 1000, 1)
    emit("RESULT " + json.dumps(fields))


def read_exact(n: int) -> bytes:
    buf = bytearray()
    while len(buf) < n:
        chunk = os.read(0, min(n - len(buf), 1 << 20))  # unbuffered: the rest of stdin stays for WASI
        if not chunk:
            raise EOFError("input ended early")
        buf += chunk
    return bytes(buf)


def read_frame() -> tuple[dict, bytes]:
    (size,) = struct.unpack("<I", read_exact(4))
    if not 0 < size <= MAX_HEADER:
        raise ValueError("bad header size")
    header = json.loads(read_exact(size))
    module_len = int(header["module_len"])
    if not 0 < module_len <= MAX_MODULE:
        raise ValueError("bad module size")
    return header, read_exact(module_len)


def main(argv: list[str]) -> int:
    # WMI is blocked in the container; without this, platform.machine() spawns `cmd /c ver` three times
    # (refused by the job) and wasmtime refuses to load. Must happen before `import wasmtime`.
    import platform

    machine = os.environ.get("PROCESSOR_ARCHITECTURE", "AMD64")
    platform.machine = lambda: machine
    if len(argv) == 2 and argv[0] == "--site":
        sys.path.insert(0, argv[1])
    try:
        header, wasm = read_frame()
        args = [str(a) for a in header.get("args", [])][:16]
        memory_mb = int(header["memory_mb"])
        fuel = int(header.get("fuel") or 0)
        epoch_s = float(header.get("epoch_seconds") or 0)
        stdout_cap = int(header["stdout_cap"])
        stderr_cap = int(header["stderr_cap"])
    except (ValueError, KeyError, TypeError, EOFError, struct.error) as exc:
        result(outcome="error", detail=f"bad sandbox input: {type(exc).__name__}")
        return 2

    import wasmtime as w

    emit("STAGE imported")
    cfg = w.Config()
    cfg.max_wasm_stack = STACK_BYTES
    cfg.consume_fuel = fuel > 0
    cfg.epoch_interruption = epoch_s > 0
    cfg.parallel_compilation = False
    engine = w.Engine(cfg)
    try:
        w.Module.validate(engine, wasm)
        module = w.Module(engine, wasm)
    except w.WasmtimeError as exc:
        result(outcome="invalid", detail=str(exc).strip().splitlines()[0][:300] if str(exc).strip() else "invalid module")
        return 0
    del wasm

    refused = [f"{imp.module}.{imp.name}" for imp in module.imports
               if imp.module != WASI or imp.name not in ALLOWED_IMPORTS or not isinstance(imp.type, w.FuncType)]
    if refused:
        result(outcome="refused", detail="imports not allowed: " + ", ".join(refused[:16]))
        return 0
    if not any(e.name == "_start" and isinstance(e.type, w.FuncType) for e in module.exports):
        result(outcome="invalid", detail="module has no _start function (build it as a WASI command)")
        return 0

    out_n, err_n, err_buf = [0], [0], bytearray()

    def on_stdout(data: bytes) -> int:
        out_n[0] += len(data)
        if out_n[0] > stdout_cap:
            result(outcome="stdout_cap", detail=f"stdout over {stdout_cap} bytes")
            os._exit(3)
        view = memoryview(data)
        while view:
            view = view[os.write(1, view):]
        return len(data)

    def on_stderr(data: bytes) -> int:
        err_n[0] += len(data)
        room = stderr_cap - len(err_buf)
        if room > 0:
            err_buf.extend(data[:room])
        return len(data)

    wasi = w.WasiConfig()
    wasi.argv = ["module", *args]
    wasi.inherit_stdin()
    wasi.stdout_custom = on_stdout
    wasi.stderr_custom = on_stderr

    store = w.Store(engine)
    store.set_limits(memory_size=memory_mb * 1024 * 1024, table_elements=100_000, instances=1, tables=1, memories=1)
    store.set_wasi(wasi)
    if fuel:
        store.set_fuel(fuel)
    stop_timer = threading.Event()
    if epoch_s > 0:
        store.set_epoch_deadline(1)

        def one_shot() -> None:
            # One increment at a monotonic deadline. Periodic ticks drift badly on Windows (~15.6 ms sleeps).
            end = time.monotonic() + epoch_s
            while (left := end - time.monotonic()) > 0:
                if stop_timer.wait(left):
                    return
            engine.increment_epoch()

        threading.Thread(target=one_shot, daemon=True).start()

    linker = w.Linker(engine)
    linker.define_wasi()
    try:
        instance = linker.instantiate(store, module)
    except (w.WasmtimeError, w.Trap) as exc:
        result(outcome="link_error", detail=str(exc).strip().splitlines()[0][:300] if str(exc).strip() else "link error")
        return 0
    start = instance.exports(store)["_start"]
    emit("STAGE running")
    outcome, detail, exit_code = "ok", "", 0
    t_run = time.perf_counter()
    try:
        start(store)
    except w.ExitTrap as exc:
        exit_code = exc.code
        outcome = "ok" if exc.code == 0 else "exit"
    except w.Trap as exc:
        code = exc.trap_code
        outcome, detail = "trap", getattr(code, "name", str(code))
    except w.WasmtimeError as exc:
        outcome, detail = "error", " | ".join(x.strip() for x in str(exc).splitlines() if x.strip())[-300:]
    run_ms = (time.perf_counter() - t_run) * 1000
    stop_timer.set()
    fuel_used = fuel - store.get_fuel() if fuel else None
    result(outcome=outcome, detail=detail, exit_code=exit_code, fuel_used=fuel_used,
           stdout_bytes=out_n[0], stderr_bytes=err_n[0], stderr_b64=base64.b64encode(bytes(err_buf)).decode(),
           t_run_ms=round(run_ms, 1), wasmtime=getattr(w, "__version__", "?"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
