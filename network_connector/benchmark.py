"""Fixed-work comparisons orchestrated by the controller, with persisted timings."""
from __future__ import annotations

import csv
import html
import io
import json
import math
import random
import statistics
import threading
import time
from collections import defaultdict
from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field

from . import jobs
from .store import Store


class BenchmarkSpec(BaseModel):
    model_config = ConfigDict(extra="forbid", allow_inf_nan=False)
    label: str = Field(default="", max_length=120)
    workload: Literal["hash_chain", "prime_sieve"] = "hash_chain"
    size: Literal["quick", "standard", "long"] = "quick"
    units: int | None = Field(default=None, ge=1, le=20000)
    seed: int = Field(default=0, ge=0, le=2**32)
    scenarios: list[str] = Field(default_factory=lambda: ["each", "all"], min_length=1, max_length=64)
    repeats: int = Field(default=3, ge=1, le=5)
    order: Literal["fixed", "shuffled"] = "fixed"
    idempotency_key: str | None = Field(default=None, max_length=120)


def calibrated_units(seconds_per_unit: list[float], target_s: float) -> int:
    return min(20000, max(1, math.ceil(target_s / min(seconds_per_unit))))


def derive(runs: list[dict[str, Any]], units: int) -> dict[str, Any]:
    grouped: dict[str, list[dict]] = defaultdict(list)
    for run in runs:
        grouped[run["scenario"]].append(run)
    scenarios = []
    for name, rows in grouped.items():
        walls = [r["wall_s"] for r in rows]
        wall = statistics.median(walls)
        representative = min(rows, key=lambda r: abs(r["wall_s"] - wall))
        scenarios.append({"scenario": name, "wall_s": wall, "min_s": min(walls), "max_s": max(walls), "units_per_s": units / wall if wall else 0, "per_worker": representative["per_worker"]})
    alone = [s for s in scenarios if s["scenario"] != "all"]
    fastest = max((s["units_per_s"] for s in alone), default=0)
    baseline = min((s["wall_s"] for s in alone), default=0)
    combined_workers = {name for r in runs if r["scenario"] == "all" for name in r["per_worker"]}
    measured_workers = {s["scenario"] for s in alone}
    ideal = sum(s["units_per_s"] for s in alone) / fastest if fastest and combined_workers <= measured_workers else None
    for s in scenarios:
        s["speedup"] = baseline / s["wall_s"] if baseline and s["wall_s"] else None
        s["relative_speed"] = s["units_per_s"] / fastest if fastest else None
    combined = next((s for s in scenarios if s["scenario"] == "all"), None)
    return {"units": units, "scenarios": scenarios, "ideal_speedup": ideal, "speedup": combined["speedup"] if combined else None, "efficiency": combined["speedup"] / ideal if combined and ideal else None, "answers_identical": len({json.dumps(r["answer"], sort_keys=True) for r in runs}) == 1, "note": "The controller computer also runs the controller; its measurements include that cost. Efficiency requires an alone measurement for every combined participant."}


class BenchmarkRunner:
    def __init__(self, store: Store, offline_s: float = 30, settle_s: float = 10, settle_timeout_s: float = 120) -> None:
        self.store, self.offline_s = store, offline_s
        self.settle_s, self.settle_timeout_s = settle_s, settle_timeout_s
        self._lock = threading.RLock()
        self._events: dict[str, threading.Event] = {}
        self._batches: dict[str, str] = {}
        self._threads: dict[str, threading.Thread] = {}
        self._stopping = threading.Event()
        store.interrupt_benchmarks()

    def _workers(self) -> list[dict]:
        return [w for w in self.store.list_workers() if w["state"] == "active" and w["last_seen"] and time.time() - w["last_seen"] < self.offline_s]

    def start(self, spec: dict | BenchmarkSpec) -> str:
        spec = spec if isinstance(spec, BenchmarkSpec) else BenchmarkSpec.model_validate(spec)
        workers = self._workers()
        names = {w["name"] for w in workers}
        if not workers:
            raise ValueError("No active online workers. Start a worker before benchmarking.")
        if any(len(s) > 63 or s not in names | {"each", "all"} for s in spec.scenarios):
            raise ValueError("Scenarios must be each, all, or the name of an active online worker.")
        with self._lock:
            if any(t.is_alive() for t in self._threads.values()):
                if spec.idempotency_key:
                    old = self.store._q("select id from benchmarks where idem_key=?", (spec.idempotency_key,))
                    if old:
                        return old[0]["id"]
                raise ValueError("A benchmark is already running. Wait or cancel it first.")
            ident, created = self.store.create_benchmark(spec.model_dump(), spec.idempotency_key)
            if not created:
                return ident
            event = self._events[ident] = threading.Event()
            thread = threading.Thread(target=self._run, args=(ident, spec, workers, event), name=f"benchmark-{ident}", daemon=True)
            self._threads[ident] = thread
            thread.start()
            return ident

    def cancel(self, ident: str) -> bool:
        with self._lock:
            event = self._events.get(ident)
            if not event:
                return False
            event.set()
            if ident in self._batches:
                self.store.cancel_batch(self._batches[ident])
            return True

    def stop(self) -> None:
        self._stopping.set()
        for event in self._events.values():
            event.set()
        for thread in self._threads.values():
            if thread is not threading.current_thread():
                thread.join(timeout=5)

    def _check(self, event: threading.Event) -> None:
        if event.is_set() or self._stopping.is_set():
            raise InterruptedError

    def _settle(self, targets: list[dict], event: threading.Event) -> list[str]:
        deadline, quiet = time.monotonic() + self.settle_timeout_s, None
        while time.monotonic() < deadline:
            self._check(event)
            current = {w["id"]: w for w in self._workers()}
            ready = all(w["id"] in current and current[w["id"]]["running"] == 0 and current[w["id"]]["metrics"].get("cpu_percent", 100) < 20 for w in targets)
            quiet = (quiet or time.monotonic()) if ready else None
            if quiet is not None and time.monotonic() - quiet >= self.settle_s:
                return []
            event.wait(0.25)
        return ["Workers did not remain idle below 20% CPU during the settling period."]

    @staticmethod
    def _slots(worker: dict) -> int:
        limits = worker["info"].get("limits", {})
        return max(1, min(limits.get("max_cpu_threads", 1), limits.get("max_concurrent_jobs", 1)))

    def _progress(self, ident: str, **progress: Any) -> None:
        current = self.store.benchmark(ident)
        result = (current.get("result") or {}) if current else {}
        self.store.update_benchmark(ident, "running", result | {"progress": progress})

    def _submit(self, ident: str, spec: BenchmarkSpec, units: int, scenario: str, rep: int, targets: list[dict], event: threading.Event) -> dict:
        self._check(event)
        params = jobs.split("benchmark", {"workload": spec.workload, "units": units, "seed": spec.seed}, min(500, 4 * sum(self._slots(w) for w in targets)))
        # Explicit targets keep unrelated workers out of an all-together comparison.
        bid, _ = self.store.create_batch("benchmark", f"bench:{ident}:{scenario}:{rep}", None, [("benchmark", p) for p in params], 86400, 1, None if scenario == "all" else scenario, eligible_workers=[w["name"] for w in targets])
        with self._lock:
            self._batches[ident] = bid
            if event.is_set():
                self.store.cancel_batch(bid)
        deadline = time.monotonic() + 86400
        last_completed = -1
        while True:
            self._check(event)
            batch = self.store.get_batch(bid)
            completed = sum(batch["counts"].get(s, 0) for s in ("succeeded", "failed", "cancelled"))
            if completed != last_completed:
                self._progress(ident, phase="calibrating" if rep < 0 else "running", scenario=scenario,
                               repeat=rep, batch_id=bid, completed_chunks=completed, total_chunks=batch["total"])
                last_completed = completed
            if batch["done"]:
                break
            if time.monotonic() > deadline:
                self.store.cancel_batch(bid)
                raise RuntimeError("Benchmark batch timed out.")
            event.wait(0.2)
        rows = self.store.batch_results(bid)
        if not all(r["status"] == "succeeded" for r in rows):
            raise RuntimeError("A benchmark chunk failed or was cancelled. No comparison was accepted.")
        records = self.store.list_jobs(batch_id=bid, limit=1000)
        started, finished = min(j["started"] for j in records), max(j["finished"] for j in records)
        per_worker = {}
        for worker in targets:
            wr = [j for j in records if j["worker_id"] == worker["id"]]
            samples = self.store.samples(worker["id"], started, finished)
            cpus = [s["cpu_percent"] for s in samples if s["cpu_percent"] is not None]
            rtts = [s["rtt_ms"] for s in samples if s["rtt_ms"] is not None]
            busy = sum(j["duration"] or 0 for j in wr)
            cpu_seconds = sum(j["cpu_seconds"] for j in wr) if wr and all(j["cpu_seconds"] is not None for j in wr) else None
            peak_mem = max(j["peak_mem_mb"] for j in wr) if wr and all(j["peak_mem_mb"] is not None for j in wr) else None
            per_worker[worker["name"]] = {"chunks": len(wr), "units": sum(p["units"] for p, j in zip(params, sorted(records, key=lambda j: j["idx"])) if j["worker_id"] == worker["id"]), "busy_seconds": busy, "cpu_seconds": cpu_seconds, "peak_mem_mb": peak_mem, "cpu_avg": statistics.mean(cpus) if cpus else None, "cpu_max": max(cpus) if cpus else None, "rtt_ms": statistics.mean(rtts) if rtts else worker["metrics"].get("rtt_ms"), "queue_wait_s": statistics.mean(j["queue_wait_s"] or 0 for j in wr) if wr else 0, "share": len(wr) / len(records), "overhead_s": max(0, finished - started - busy / self._slots(worker))}
        settings = {w["name"]: {"cpu_name": w["info"].get("cpu_name"), "ram_total_mb": w["info"].get("ram_total_mb"), "limits": w["info"].get("limits", {}), "link": w.get("link", "unknown"), "version": w["version"], "metrics": w["metrics"]} for w in targets}
        return {"scenario": scenario, "repeat": rep, "batch_id": bid, "started": started, "finished": finished, "wall_s": max(finished - started, 1e-9), "settings": settings, "per_worker": per_worker, "answer": jobs.combine("benchmark", [r["result"] for r in rows]), "valid": True}

    def _run(self, ident: str, spec: BenchmarkSpec, workers: list[dict], event: threading.Event) -> None:
        runs, warnings = [], []
        try:
            scenarios = []
            for scenario in spec.scenarios:
                scenarios.extend([w["name"] for w in workers] if scenario == "each" else [scenario])
            scenarios = list(dict.fromkeys(scenarios))
            selected = workers if "all" in scenarios else [w for w in workers if w["name"] in scenarios]
            units = spec.units
            if units is None:
                calibration = [self._submit(ident, spec, 2, w["name"], -1, [w], event)["wall_s"] / 2 for w in selected]
                units = calibrated_units(calibration, {"quick": 30, "standard": 300, "long": 900}[spec.size])
            schedule = [(scenario, rep) for rep in range(1, spec.repeats + 1) for scenario in scenarios]
            if spec.order == "shuffled":
                random.Random(spec.seed).shuffle(schedule)
            for scenario, rep in schedule:
                targets = workers if scenario == "all" else [w for w in workers if w["name"] == scenario]
                self._progress(ident, phase="settling", scenario=scenario, repeat=rep,
                               completed_runs=len(runs), total_runs=len(schedule))
                before = {w["name"]: w["metrics"].get("cpu_percent") for w in self.store.list_workers()}
                warnings.extend(self._settle(targets, event))
                current = {w["id"]: w for w in self.store.list_workers()}
                targets = [current[w["id"]] for w in targets]
                warnings.extend(f"{w['name']} is on battery power." for w in targets if w["metrics"].get("on_ac_power") is False)
                run = self._submit(ident, spec, units, scenario, rep, targets, event)
                for name, settings in run["settings"].items():
                    settings["foreground_cpu_before"] = before.get(name)
                run["valid"] = not runs or run["answer"] == runs[0]["answer"]
                self.store.add_benchmark_run(ident, run)
                runs.append(run)
                self.store.update_benchmark(ident, "running", derive(runs, units) | {"warnings": list(dict.fromkeys(warnings))})
            result = derive(runs, units) | {"warnings": list(dict.fromkeys(warnings))}
            self.store.update_benchmark(ident, "finished" if result["answers_identical"] else "invalid", result)
        except InterruptedError:
            bid = self._batches.get(ident)
            if bid:
                self.store.cancel_batch(bid)
            self.store.update_benchmark(ident, "interrupted" if self._stopping.is_set() else "cancelled", derive(runs, spec.units or 0) if runs else None)
        except Exception as exc:
            self.store.update_benchmark(ident, "failed", error=str(exc)[:2000])
        finally:
            with self._lock:
                self._batches.pop(ident, None)
                self._events.pop(ident, None)


def shared_result(store: Store, worker_name: str) -> dict[str, Any] | None:
    benchmark = store.latest_participant_benchmark(worker_name)
    if benchmark:
        hardware = {}
        for run in benchmark["runs"]:
            for name, setting in run["settings"].items():
                hardware[name] = {"cpu_name": setting.get("cpu_name"), "ram_total_mb": setting.get("ram_total_mb"), "threads": setting.get("limits", {}).get("max_cpu_threads")}
        result = {"id": benchmark["id"], "created": benchmark["created"], "label": benchmark["label"], "result": benchmark["result"], "workers": hardware}
        if len(json.dumps(result).encode()) > 65000:
            for scenario in result["result"].get("scenarios", []):
                scenario["per_worker"] = {name: {k: v for k, v in data.items() if k in ("chunks", "units", "share")} for name, data in scenario.get("per_worker", {}).items()}
        if len(json.dumps(result).encode()) <= 65536:
            return result
    return None


def export_benchmark(benchmark: dict[str, Any], format: str = "json") -> str:
    if format == "json":
        return json.dumps(benchmark, indent=2)
    rows = (benchmark.get("result") or {}).get("scenarios", [])
    fields = ["scenario", "wall_s", "min_s", "max_s", "units_per_s", "speedup", "relative_speed"]
    if format == "csv":
        stream = io.StringIO(newline="")
        writer = csv.DictWriter(stream, fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)
        return stream.getvalue()
    if format != "html":
        raise ValueError("Export format must be json, csv or html")
    cells = "".join("<tr>" + "".join(f"<td>{html.escape(str(row.get(f, '')))}</td>" for f in fields) + "</tr>" for row in rows)
    maximum = max((r["wall_s"] for r in rows), default=1)
    bars = "".join(f'<text x="0" y="{i * 36 + 18}">{html.escape(r["scenario"])}</text><rect x="130" y="{i * 36}" width="{450 * r["wall_s"] / maximum}" height="24" fill="#2867b2"/>' for i, r in enumerate(rows))
    return f'<!doctype html><html lang="en"><meta charset="utf-8"><title>Benchmark {html.escape(benchmark["id"])}</title><style>body{{font:16px system-ui;margin:40px;max-width:1100px}}td,th{{padding:9px;text-align:left;border-bottom:1px solid #ddd}}svg{{width:100%;max-width:650px}}</style><h1>{html.escape(benchmark.get("label") or "Network Connector benchmark")}</h1><p>Status: {html.escape(benchmark["status"])}</p><svg viewBox="0 0 650 {max(40, len(rows) * 36)}">{bars}</svg><table><tr>{"".join(f"<th>{f}</th>" for f in fields)}</tr>{cells}</table><h2>Measurement details</h2><pre>{html.escape(json.dumps(benchmark, indent=2))}</pre></html>'
