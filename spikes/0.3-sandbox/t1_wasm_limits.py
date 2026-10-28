"""Throwaway: item 1. wasmtime limits and WASI surface, each case run inside AppContainer + Job Object."""
from __future__ import annotations

import json
import os

import ac
import wat_modules as m

(ac.HERE / "build" / "garbage.wasm").write_bytes(b"\x00asm\x01\x00\x00\x00" + os.urandom(64))

CASES = [
    ("probe (all-wasi allowlist)", "probe.wasm", ["--allow", "all-wasi"], b"", 20),
    ("probe (strict allowlist)", "probe.wasm", [], b"", 20),
    ("import env.system", "bad_env.wasm", ["--allow", "all-wasi"], b"", 20),
    ("import wasi sock_open (not in p1)", "bad_wasi.wasm", ["--allow", "all-wasi"], b"", 20),
    ("not valid wasm", "garbage.wasm", [], b"", 20),
    ("memory bomb, store limit 64 MB", "membomb.wasm", ["--mem-mb", "64"], b"", 20),
    ("deep recursion, stack 512 KB", "recurse.wasm", ["--stack-kb", "512"], b"", 20),
    ("infinite loop, fuel 2e9", "loop.wasm", ["--fuel", "2000000000"], b"", 30),
    ("infinite loop, epoch 1.0 s", "loop.wasm", ["--seconds", "1"], b"", 30),
    ("stdout flood, cap 1 MB", "huge_out.wasm", ["--stdout-cap", str(1 << 20), "--quiet-stdout"], b"", 30),
    ("poll_oneoff sleep 10 s, epoch 1 s, outer kill 3 s", "sleep.wasm", ["--seconds", "1"], b"", 3),
    ("sha_chain 1000 (control)", "sha_chain.wasm", [], m.sha_chain_input(1000), 20),
]


def result_line(stderr: bytes) -> dict:
    for line in reversed(stderr.decode(errors="replace").splitlines()):
        if line.startswith("RESULT "):
            return json.loads(line[7:])
    return {"raw_stderr_tail": stderr.decode(errors="replace")[-300:]}


def main() -> None:
    for title, module, args, stdin, timeout in CASES:
        r = ac.run(ac.runner_cmd(module, *args), stdin, timeout=timeout, mem_mb=512)
        res = result_line(r.stderr)
        keep = {k: res.get(k) for k in ("outcome", "detail", "exit_code", "refused_imports", "error", "fuel_used",
                                        "memory_pages", "stdout_bytes", "t_run_ms") if res.get(k) not in (None, "")}
        out = r.stdout[:80]
        print(f"## {title}\n   process exit={r.exit_code:#x} timed_out={r.timed_out} wall={r.t['process_ms']:.0f} ms "
              f"peak={r.peak_mb:.0f} MB\n   result={json.dumps(keep)}\n   stdout={out!r}")
    print("leftovers:", ac.leftovers())


if __name__ == "__main__":
    main()
