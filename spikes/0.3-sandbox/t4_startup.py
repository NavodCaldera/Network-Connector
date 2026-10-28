"""Throwaway: item 4. Per-job sandbox start-up cost, 20 runs per scenario (1 warm-up run discarded).

Timer: time.perf_counter() in the launching process around each Win32 call (this is a spike measurement,
not a benchmark result). Every run must echo its input back exactly, or the scenario is INVALID.
"""
from __future__ import annotations

import json
import statistics
import sys

import ac

N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
PAYLOAD = b"nc-sandbox-startup-check\n"
ECHO = str(ac.HERE / "build" / "echo.wasm")


def runner_internal(stderr: bytes) -> dict:
    for line in reversed(stderr.decode(errors="replace").splitlines()):
        if line.startswith("RESULT "):
            return json.loads(line[7:])
    return {}


SCENARIOS = [
    ("A cold per job: profile+launch+python+wasmtime+run+delete", dict(), []),
    ("B as A with --fast-platform", dict(), ["--fast-platform"]),
    ("C Job only, no AppContainer (isolates AppContainer cost), --fast-platform", dict(appcontainer=False,
                                                                                      cwd=str(ac.HERE / "build")),
     ["--fast-platform"]),
    ("D pool: sandbox pre-started (READY), then job sent, --fast-platform", dict(wait_ready=True),
     ["--fast-platform", "--pool"]),
]


def stats(xs: list[float]) -> str:
    xs = sorted(xs)
    p90 = xs[min(len(xs) - 1, round(0.9 * (len(xs) - 1)))]
    return f"median {statistics.median(xs):7.1f}  p90 {p90:7.1f}  min {xs[0]:7.1f}  max {xs[-1]:7.1f}"


def main() -> None:
    for title, kw, extra in SCENARIOS:
        rows: dict[str, list[float]] = {}
        valid = True
        for i in range(N + 1):
            if "--pool" in extra:
                cmd = [ac.PY_BASE, "-I", "-S", "-B", str(ac.HERE / "runner.py"), "--site", ac.SITE, *extra]
                stdin = ECHO.encode() + b"\n" + PAYLOAD
            else:
                cmd = ac.runner_cmd("echo.wasm", *extra)
                stdin = PAYLOAD
            r = ac.run(cmd, stdin, **kw)
            if r.stdout != PAYLOAD or r.exit_code != 0 or r.leftover_after_delete:
                valid = False
                print("   bad run:", r.exit_code, r.stdout[:60], r.stderr[-300:], r.leftover_after_delete)
            if i == 0:
                continue
            for k, v in r.t.items():
                rows.setdefault(k, []).append(v)
            inside = runner_internal(r.stderr)
            for k in ("t_python_ms", "t_import_ms", "t_compile_ms", "t_run_ms", "t_total_ms"):
                if k in inside:
                    rows.setdefault("runner." + k, []).append(inside[k])
        print(f"## {title}  [{'VALID' if valid else 'INVALID'}; n={N}]")
        for k, v in rows.items():
            print(f"   {k:22s} {stats(v)}")
    print("leftovers:", ac.leftovers())


if __name__ == "__main__":
    main()
