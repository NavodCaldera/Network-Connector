"""Throwaway: the in-sandbox Wasm runner for the 0.3 spike (prototype of sandbox_runner.py).

Launched as `python.exe -I -S -B runner.py --site <site-packages> --module x.wasm ...`.
-I/-S mean no PYTHONPATH, no user site, no venv redirector; --site is added to sys.path by hand.
WASI: stdin inherited from this process, stdout/stderr captured with caps, nothing pre-opened,
no environment, argv = ["module"]. The last stderr line is `RESULT {json}`.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import threading
import time

T_START = time.perf_counter()

ALLOW_STRICT = frozenset({
    "fd_read", "fd_write", "fd_close", "fd_fdstat_get", "fd_seek", "fd_prestat_get", "fd_prestat_dir_name",
    "args_get", "args_sizes_get", "environ_get", "environ_sizes_get", "clock_time_get", "clock_res_get",
    "random_get", "proc_exit", "sched_yield", "poll_oneoff",
})


def parse() -> argparse.Namespace:
    ap = argparse.ArgumentParser()
    ap.add_argument("--site", default="")
    ap.add_argument("--module", default="")
    ap.add_argument("--pool", action="store_true", help="start, import, print READY, then read module path + input")
    ap.add_argument("--fast-platform", action="store_true", help="skip WMI / cmd /c ver probing in platform")
    ap.add_argument("--mem-mb", type=int, default=64)
    ap.add_argument("--fuel", type=int, default=0)
    ap.add_argument("--seconds", type=float, default=0.0, help="epoch deadline; 0 = no epoch")
    ap.add_argument("--tick-ms", type=float, default=0.0, help="0 = one-shot timer at the deadline")
    ap.add_argument("--stack-kb", type=int, default=512)
    ap.add_argument("--stdout-cap", type=int, default=1 << 20)
    ap.add_argument("--stderr-cap", type=int, default=64 << 10)
    ap.add_argument("--allow", choices=("strict", "all-wasi"), default="strict")
    ap.add_argument("--quiet-stdout", action="store_true", help="count stdout bytes but don't forward them")
    return ap.parse_args()


def result(**fields: object) -> None:
    fields["t_total_ms"] = round((time.perf_counter() - T_START) * 1000, 2)
    sys.stderr.write("RESULT " + json.dumps(fields) + "\n")
    sys.stderr.flush()


def main() -> int:
    a = parse()
    if a.fast_platform:
        import platform
        platform.machine = lambda: "AMD64"  # wasmtime._ffi only needs this; avoids WMI + 3 blocked spawns
    if a.site:
        sys.path.insert(0, a.site)
    t0 = time.perf_counter()
    import wasmtime as w
    t_import = time.perf_counter()

    cfg = w.Config()
    cfg.max_wasm_stack = a.stack_kb * 1024
    cfg.consume_fuel = a.fuel > 0
    cfg.epoch_interruption = a.seconds > 0
    cfg.parallel_compilation = False
    engine = w.Engine(cfg)
    t_engine = time.perf_counter()

    if a.pool:
        sys.stderr.write("READY\n")
        sys.stderr.flush()
        line = bytearray()
        while (ch := os.read(0, 1)) not in (b"", b"\n"):  # unbuffered: the rest of stdin stays for WASI
            line += ch
        a.module = line.decode()
    t_job = time.perf_counter()
    with open(a.module, "rb") as f:
        wasm = f.read()
    try:
        w.Module.validate(engine, wasm)
        module = w.Module(engine, wasm)
    except w.WasmtimeError as exc:
        result(outcome="invalid", error=str(exc)[:300])
        return 0
    t_compile = time.perf_counter()

    refused = []
    for imp in module.imports:
        ok = imp.module == "wasi_snapshot_preview1" and isinstance(imp.type, w.FuncType)
        if a.allow == "strict":
            ok = ok and imp.name in ALLOW_STRICT
        if not ok:
            refused.append(f"{imp.module}.{imp.name}")
    if refused:
        result(outcome="refused", refused_imports=refused)
        return 0

    out_n, err_n, err_buf = [0], [0], bytearray()
    stdout = sys.stdout.buffer

    def on_stdout(data: bytes) -> int:
        out_n[0] += len(data)
        if out_n[0] > a.stdout_cap:
            # Single-use process: stop hard. The helper sees this result and the exit code.
            result(outcome="stdout_cap", stdout_bytes=out_n[0])
            os._exit(3)
        if not a.quiet_stdout:
            stdout.write(data)
        return len(data)

    def on_stderr(data: bytes) -> int:
        err_n[0] += len(data)
        room = a.stderr_cap - len(err_buf)
        if room > 0:
            err_buf.extend(data[:room])
        return len(data)

    wasi = w.WasiConfig()
    wasi.argv = ["module"]
    wasi.inherit_stdin()
    wasi.stdout_custom = on_stdout
    wasi.stderr_custom = on_stderr

    store = w.Store(engine)
    store.set_limits(memory_size=a.mem_mb * 1024 * 1024, table_elements=100_000, instances=1, tables=1, memories=1)
    store.set_wasi(wasi)
    if a.fuel:
        store.set_fuel(a.fuel)
    stop_ticks = threading.Event()
    ticker = None
    if a.seconds > 0:
        if a.tick_ms:
            # Periodic ticks (lets the helper see progress), deadline counted in ticks.
            store.set_epoch_deadline(max(1, round(a.seconds * 1000 / a.tick_ms)))

            def tick() -> None:
                while not stop_ticks.wait(a.tick_ms / 1000):
                    engine.increment_epoch()
        else:
            # One-shot: one increment when the monotonic deadline passes.
            store.set_epoch_deadline(1)

            def tick() -> None:
                end = time.monotonic() + a.seconds
                while (left := end - time.monotonic()) > 0:
                    if stop_ticks.wait(left):
                        return
                engine.increment_epoch()
        ticker = threading.Thread(target=tick, daemon=True)

    linker = w.Linker(engine)
    linker.define_wasi()
    try:
        instance = linker.instantiate(store, module)
    except (w.WasmtimeError, w.Trap) as exc:
        result(outcome="link_error", error=str(exc)[:300])
        return 0
    t_inst = time.perf_counter()

    outcome, detail, exit_code = "ok", "", 0
    start = instance.exports(store)["_start"]
    t_run0 = time.perf_counter()
    if ticker:
        ticker.start()
    try:
        start(store)
    except w.ExitTrap as exc:
        exit_code = exc.code
        outcome = "exit"
    except w.Trap as exc:
        outcome, detail = "trap", f"{exc.trap_code}"
    except w.WasmtimeError as exc:
        outcome, detail = "error", " | ".join(x.strip() for x in str(exc).splitlines() if x.strip())[-240:]
    t_run = time.perf_counter()
    stop_ticks.set()
    stdout.flush()

    mem = instance.exports(store).get("memory") if outcome != "trap" or True else None
    fuel_used = a.fuel - store.get_fuel() if a.fuel else None
    result(outcome=outcome, detail=detail, exit_code=exit_code, fuel_used=fuel_used,
           memory_pages=mem.size(store) if isinstance(mem, w.Memory) else None,
           stdout_bytes=out_n[0], stderr_bytes=err_n[0],
           t_python_ms=round((t0 - T_START) * 1000, 2),
           t_import_ms=round((t_import - t0) * 1000, 2), t_engine_ms=round((t_engine - t_import) * 1000, 2),
           t_compile_ms=round((t_compile - t_job) * 1000, 2), t_inst_ms=round((t_inst - t_compile) * 1000, 2),
           t_run_ms=round((t_run - t_run0) * 1000, 2), wasmtime=getattr(w, "__version__", "?"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
