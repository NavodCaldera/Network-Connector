"""End-to-end demo that proves which computer did the work.

Splits a prime-counting job into many chunks, lets every connected worker pull chunks, then
prints, per worker: name, CPU, memory, chunks completed, and execution time. The combined
answer is checked against a known value.
"""
from __future__ import annotations

import time
from typing import Any, Callable

from .client import AdminClient

# pi(10^8) = 5,761,455 -- a well-known value, so the demo can verify the distributed answer.
KNOWN_PRIMES_BELOW_1E8 = 5_761_455


def run_demo(
    client: AdminClient,
    out: Callable[[str], None] = print,
    chunks: int = 40,
    target: str | None = None,
    timeout_s: float = 600,
    should_stop: Callable[[], bool] = lambda: False,
) -> dict[str, Any]:
    workers = [w for w in client.workers() if w["status"] == "online"]
    if target:
        workers = [w for w in workers if w["name"] == target]
    if not workers:
        raise RuntimeError("No online worker found. Start a worker (the i3, or the local worker) and try again.")
    out("=== Connected workers ===")
    for w in workers:
        info, m = w["info"], w["metrics"]
        out(f"  {w['name']:<16} {info.get('cpu_name', '?')}  |  {info.get('cores_logical', '?')} threads, "
            f"{info.get('ram_total_mb', 0) / 1024:.1f} GB RAM  |  now: CPU {m.get('cpu_percent', 0)}%, "
            f"RAM {m.get('mem_percent', 0)}%")
    out("")
    key = f"demo-{int(time.time())}"
    sub = client.submit("prime_range", {"start": 0, "end": 100_000_000}, chunks=chunks, label="demo: count primes below 100,000,000",
                        idempotency_key=key, target=target)
    batch_id = sub["batch_id"]
    out(f"Submitted job batch {batch_id}: count primes below 100,000,000, split into {sub['chunks']} chunks"
        + (f", pinned to worker {target}" if target else ""))
    t0 = time.monotonic()
    batch: dict[str, Any] = {}
    while time.monotonic() - t0 < timeout_s:
        if should_stop():
            client.cancel_batch(batch_id)
            raise RuntimeError("Demo cancelled.")
        batch = client.batch(batch_id)
        c = batch["counts"]
        live = {w["name"]: w["metrics"] for w in client.workers() if w["status"] == "online"}
        usage = "  ".join(f"{n}: CPU {m.get('cpu_percent', 0):>5}%" for n, m in live.items())
        out(f"  progress: {c.get('succeeded', 0)}/{batch['total']} done, {c.get('running', 0)} running, "
            f"{c.get('queued', 0)} queued, {c.get('failed', 0)} failed   [{usage}]")
        if batch["done"]:
            break
        time.sleep(1.5)
    else:
        client.cancel_batch(batch_id)
        raise RuntimeError("Demo timed out and was cancelled.")
    wall = time.monotonic() - t0
    out("")
    out("=== Result ===")
    combined = batch.get("combined") or {}
    primes = combined.get("primes")
    ok = primes == KNOWN_PRIMES_BELOW_1E8
    out(f"  Primes below 100,000,000: {primes}  ->  {'CORRECT (matches the known value 5,761,455)' if ok else 'MISMATCH or incomplete'}")
    out(f"  Total time: {wall:.1f}s   (summed worker time: {sum(v['busy_seconds'] for v in batch['by_worker'].values()):.1f}s)")
    out("")
    out("=== Who did the work ===")
    names = {w["name"]: w for w in client.workers()}
    for name, stats in sorted(batch["by_worker"].items()):
        info = names.get(name, {}).get("info", {})
        out(f"  {name:<16} {stats['chunks']:>3} chunks  {stats['busy_seconds']:>7.1f}s busy  "
            f"({info.get('cpu_name', '?')}, {info.get('ram_total_mb', 0) / 1024:.1f} GB RAM)")
    return {"batch_id": batch_id, "ok": ok, "wall_seconds": wall, "by_worker": batch["by_worker"], "primes": primes}
