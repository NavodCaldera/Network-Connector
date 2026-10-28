"""Throwaway: item 5. Fuel vs epoch accuracy as a time limit, and Wasm vs native speed for a SHA-256 chain.

Timings: Wasm = runner's perf_counter around `_start` (compile excluded); native = QueryPerformanceCounter
around the loop inside the C program; hashlib = perf_counter around the loop in this process.
Every speed scenario must print the reference digest or it is INVALID.
"""
from __future__ import annotations

import hashlib
import json
import statistics
import subprocess
import time

import ac
import wat_modules as m

ROUNDS = 2_000_000
REPS = 7


def res(stderr: bytes) -> dict:
    for line in reversed(stderr.decode(errors="replace").splitlines()):
        if line.startswith("RESULT "):
            return json.loads(line[7:])
    return {"tail": stderr.decode(errors="replace")[-200:]}


def wasm(module: str, args: list[str], stdin: bytes = b"", timeout: float = 60) -> tuple[ac.Run, dict]:
    r = ac.run(ac.runner_cmd(module, "--fast-platform", *args), stdin, timeout=timeout)
    return r, res(r.stderr)


def speed() -> None:
    ref = m.sha_chain_reference(ROUNDS)
    inp = m.sha_chain_input(ROUNDS)
    print(f"## Speed: SHA-256 chain, {ROUNDS:,} rounds of one 64-byte block, reference {ref[:16]}...")
    rows: dict[str, list[float]] = {}
    valid: dict[str, bool] = {}

    def add(name: str, ms: float, out: str) -> None:
        rows.setdefault(name, []).append(ms)
        valid[name] = valid.get(name, True) and out.strip() == ref

    for _ in range(REPS):
        for exe in ("sha_chain_O2.exe", "sha_chain_native.exe"):
            p = subprocess.run([str(ac.HERE / "build" / exe)], input=inp, capture_output=True)
            add(f"native gcc {exe[10:-4]}", float(p.stderr.split()[1]), p.stdout.decode())
        for label, extra in (("wasm in sandbox, no fuel/epoch", []), ("wasm in sandbox, epoch on", ["--seconds", "600"]),
                             ("wasm in sandbox, fuel on", ["--fuel", str(10 ** 15)])):
            r, d = wasm("sha_chain.wasm", extra, inp)
            add(label, d.get("t_run_ms", float("nan")), r.stdout.decode())
            if extra and extra[0] == "--fuel":
                rows.setdefault("_fuel_per_round", []).append(d["fuel_used"] / ROUNDS)
        t = time.perf_counter()
        v = b"\x00" * 32
        for _ in range(ROUNDS):
            v = hashlib.sha256(v).digest()
        add("python hashlib (OpenSSL)", (time.perf_counter() - t) * 1000, v.hex())
    base = statistics.median(rows["native gcc O2"])
    for name, xs in rows.items():
        if name.startswith("_"):
            continue
        med = statistics.median(xs)
        print(f"   {name:34s} median {med:8.1f} ms  ({ROUNDS / med / 1000:6.2f} M rounds/s)  "
              f"x{med / base:5.2f} vs gcc -O2  [{'VALID' if valid[name] else 'INVALID'}]  runs={[round(x) for x in xs]}")
    print(f"   fuel per SHA round: {statistics.median(rows['_fuel_per_round']):.0f}")


def epoch_accuracy() -> None:
    print("## Epoch as a time limit (loop.wasm, inside sandbox)")
    for limit, tick in ((0.25, "0"), (1.0, "0"), (3.0, "0"), (1.0, "10"), (1.0, "1")):
        xs = []
        for _ in range(REPS):
            _, d = wasm("loop.wasm", ["--seconds", str(limit), "--tick-ms", tick])
            assert d.get("detail") == "TrapCode.INTERRUPT", d
            xs.append(d["t_run_ms"])
        mode = "one-shot timer" if tick == "0" else f"periodic {tick} ms ticks"
        print(f"   limit {limit:4.2f} s, {mode:22s} stopped after median {statistics.median(xs):7.1f} ms "
              f"(min {min(xs):.1f}, max {max(xs):.1f}), error {statistics.median(xs) - limit * 1000:+.1f} ms")
    _, d = wasm("huge_out.wasm", ["--seconds", "1", "--quiet-stdout", "--stdout-cap", str(10 ** 15)])
    print(f"   limit 1.00 s on a host-call-heavy module (64 KiB fd_write per iteration): {d.get('detail')} after "
          f"{d.get('t_run_ms')} ms")


def fuel_accuracy() -> None:
    print("## Fuel as a time limit")
    _, d = wasm("loop.wasm", ["--fuel", str(2 * 10 ** 9)])
    loop_rate = d["fuel_used"] / (d["t_run_ms"] / 1000)
    _, d = wasm("sha_chain.wasm", ["--fuel", str(10 ** 15)], m.sha_chain_input(500_000))
    sha_rate = d["fuel_used"] / (d["t_run_ms"] / 1000)
    _, d = wasm("huge_out.wasm", ["--fuel", str(10 ** 15), "--seconds", "1", "--quiet-stdout",
                                  "--stdout-cap", str(10 ** 15)])
    io_rate = d["fuel_used"] / (d["t_run_ms"] / 1000)
    print(f"   fuel/s: integer loop {loop_rate:.3g}, sha_chain {sha_rate:.3g}, fd_write-heavy {io_rate:.3g}")
    budget = int(loop_rate)  # "1 second", calibrated on the integer loop
    for title, module, stdin, extra in (
            ("integer loop", "loop.wasm", b"", []),
            ("sha_chain", "sha_chain.wasm", m.sha_chain_input(10 ** 9), []),
            ("fd_write-heavy", "huge_out.wasm", b"", ["--quiet-stdout", "--stdout-cap", str(10 ** 15)])):
        xs = []
        for _ in range(3 if module != "huge_out.wasm" else 1):
            r, d = wasm(module, ["--fuel", str(budget), *extra], stdin, timeout=10)
            if r.timed_out:
                print(f"   budget {budget:.3g} fuel (= 1 s of integer loop) on {title:15s}: NOT stopped by fuel; "
                      f"outer wall-clock kill at {r.t['process_ms']:.0f} ms (exit {r.exit_code:#x}); "
                      f"fuel would last ~{budget / io_rate:.0f} s")
                break
            assert d.get("detail") == "TrapCode.OUT_OF_FUEL", d
            xs.append(d["t_run_ms"])
        else:
            print(f"   budget {budget:.3g} fuel (= 1 s of integer loop) on {title:15s}: ran {statistics.median(xs):9.1f} ms")
    _, d = wasm("sleep.wasm", ["--fuel", str(budget)], timeout=15)
    print(f"   same budget on poll_oneoff(10 s sleep): {d.get('outcome')} after {d.get('t_run_ms')} ms, "
          f"fuel used {d.get('fuel_used')}")


if __name__ == "__main__":
    import sys
    parts = sys.argv[1:] or ["speed", "epoch", "fuel"]
    if "speed" in parts:
        speed()
    if "epoch" in parts:
        epoch_accuracy()
    if "fuel" in parts:
        fuel_accuracy()
    print("leftovers:", ac.leftovers())
