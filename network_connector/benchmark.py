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

import base64
import binascii
import hashlib
import re
from importlib import resources

from pydantic import BaseModel, ConfigDict, Field, model_validator

from . import jobs
from .store import Store

# The Part 1 hash_chain compiled to Wasm (examples/wasm/bench, Rust, std only). Pinned: a different file is refused.
WASM_BENCH_SHA256 = "245016c78af96ff4ec963b6bbd121119a2e5e5520166b20cfc7c26c1eee64257"
WASM_BENCH_LIMITS = {"memory_mb": 64, "seconds": 3600}
_TOP = re.compile(r"^top([1-9][0-9]?)$")


def wasm_bench_module() -> bytes:
    data = resources.files("network_connector").joinpath("data/wasm_bench.wasm").read_bytes()
    if hashlib.sha256(data).hexdigest() != WASM_BENCH_SHA256:
        raise ValueError("The built-in wasm_bench module does not match its pinned hash. Reinstall Network Connector.")
    return data


def wasm_bench_input(chunk: dict[str, Any]) -> str:
    return base64.b64encode(f"{chunk['seed']} {chunk['start']} {chunk['units']}".encode("ascii")).decode("ascii")


def wasm_bench_answer(result: dict[str, Any]) -> dict[str, Any]:
    """The wasm module's stdout, checked to have the native result's exact shape."""
    try:
        data = json.loads(base64.b64decode(result["stdout_b64"], validate=True))
    except (KeyError, TypeError, ValueError, binascii.Error) as exc:
        raise RuntimeError("A wasm_bench chunk returned unreadable output.") from exc
    if (result.get("exit_code") not in (0, None) or not isinstance(data, dict) or set(data) != {"units", "digest"}
            or not isinstance(data["units"], int) or not re.fullmatch(r"[0-9a-f]{64}", str(data["digest"]))):
        raise RuntimeError("A wasm_bench chunk returned output in the wrong format.")
    return data


def scenario_parts(name: str) -> tuple[str, str]:
    """('all', 'wasm') for 'all@wasm'; ('i3', 'native') for 'i3'."""
    base, _, engine = name.partition("@")
    return base, engine or "native"


def is_alone(base: str) -> bool:
    return base != "all" and not _TOP.match(base)


class BenchmarkSpec(BaseModel):
    model_config = ConfigDict(extra="forbid", allow_inf_nan=False)
    label: str = Field(default="", max_length=120)
    workload: Literal["hash_chain", "prime_sieve"] = "hash_chain"
    size: Literal["quick", "standard", "long"] = "quick"
    units: int | None = Field(default=None, ge=1, le=20000)
    seed: int = Field(default=0, ge=0, le=2**32)
    # each | all | top<N> (the N fastest by calibration) | a worker's name
    scenarios: list[str] = Field(default_factory=lambda: ["each", "all"], min_length=1, max_length=64)
    # native = the built-in benchmark job; wasm = the same work as a sandboxed wasm job (PLAN §19)
    engines: list[Literal["native", "wasm"]] = Field(default_factory=lambda: ["native"], min_length=1, max_length=2)
    repeats: int = Field(default=3, ge=1, le=5)
    order: Literal["fixed", "shuffled"] = "fixed"
    idempotency_key: str | None = Field(default=None, max_length=120)

    @model_validator(mode="after")
    def _wasm_runs_hash_chain(self) -> "BenchmarkSpec":
        self.engines = list(dict.fromkeys(self.engines))
        if "wasm" in self.engines and self.workload != "hash_chain":
            raise ValueError("The wasm engine runs the hash_chain workload only.")
        return self


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
        base, engine = scenario_parts(name)
        scenarios.append({"scenario": name, "engine": engine, "wall_s": wall, "min_s": min(walls), "max_s": max(walls), "units_per_s": units / wall if wall else 0, "per_worker": representative["per_worker"]})
    by_name = {s["scenario"]: s for s in scenarios}
    native = [s for s in scenarios if s["engine"] == "native"]
    alone = [s for s in native if is_alone(s["scenario"])]
    fastest = max((s["units_per_s"] for s in alone), default=0)
    baseline = min((s["wall_s"] for s in alone), default=0)
    combined_workers = {name for r in runs if r["scenario"] == "all" for name in r["per_worker"]}
    measured_workers = {s["scenario"] for s in alone}
    ideal = sum(s["units_per_s"] for s in alone) / fastest if fastest and combined_workers <= measured_workers else None
    overheads = []
    for s in scenarios:
        own_alone = [a for a in scenarios if a["engine"] == s["engine"] and is_alone(scenario_parts(a["scenario"])[0])]
        own_baseline = min((a["wall_s"] for a in own_alone), default=0)
        own_fastest = max((a["units_per_s"] for a in own_alone), default=0)
        s["speedup"] = own_baseline / s["wall_s"] if own_baseline and s["wall_s"] else None
        s["relative_speed"] = s["units_per_s"] / own_fastest if own_fastest else None
        twin = by_name.get(scenario_parts(s["scenario"])[0]) if s["engine"] == "wasm" else None
        s["wasm_overhead"] = s["wall_s"] / twin["wall_s"] if twin and twin["wall_s"] else None
        if s["wasm_overhead"]:
            overheads.append(s["wasm_overhead"])
    combined = by_name.get("all")
    return {"units": units, "scenarios": scenarios, "ideal_speedup": ideal, "speedup": combined["speedup"] if combined else None, "efficiency": combined["speedup"] / ideal if combined and ideal else None,
            "wasm_overhead": statistics.median(overheads) if overheads else None,
            "answers_identical": len({json.dumps(r["answer"], sort_keys=True) for r in runs}) == 1, "note": "The controller computer also runs the controller; its measurements include that cost. Efficiency requires an alone measurement for every combined participant. Wasm overhead = wasm wall time / native wall time for the same scenario (sandbox start-up included)."}


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
        if any(len(s) > 63 or (s not in names | {"each", "all"} and not _TOP.match(s)) for s in spec.scenarios):
            raise ValueError("Scenarios must be each, all, topN (for example top3), or the name of an active online worker.")
        if "wasm" in spec.engines:
            missing = [w["name"] for w in workers if not w["info"].get("sandbox")]
            if missing:
                raise ValueError(f"These helpers can't run sandboxed wasm jobs: {', '.join(missing)}. "
                                 "Set up their sandbox (nc sandbox setup) or run the native engine only.")
            wasm_bench_module()
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

    def _submit(self, ident: str, spec: BenchmarkSpec, units: int, scenario: str, rep: int, targets: list[dict], event: threading.Event, engine: str = "native") -> dict:
        self._check(event)
        params = jobs.split("benchmark", {"workload": spec.workload, "units": units, "seed": spec.seed}, min(500, 4 * sum(self._slots(w) for w in targets)))
        if engine == "wasm":
            self.store.add_module(WASM_BENCH_SHA256, wasm_bench_module(), "wasm_bench (built in)")
            items = [("wasm", {"module_sha256": WASM_BENCH_SHA256, "input_b64": wasm_bench_input(p), "args": [],
                               "limits": dict(WASM_BENCH_LIMITS)}) for p in params]
        else:
            items = [("benchmark", p) for p in params]
        label = scenario if engine == "native" else f"{scenario}@{engine}"
        target = scenario if is_alone(scenario) and len(targets) == 1 else None
        # Explicit targets keep unrelated workers out of an all-together comparison.
        bid, _ = self.store.create_batch(items[0][0], f"bench:{ident}:{label}:{rep}", None, items, 86400, 1, target, eligible_workers=[w["name"] for w in targets])
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
        settings = {w["name"]: {"cpu_name": w["info"].get("cpu_name"), "ram_total_mb": w["info"].get("ram_total_mb"), "limits": w["info"].get("limits", {}), "link": w.get("link", "unknown"), "version": w["version"], "metrics": w["metrics"], **device_label(w)} for w in targets}
        results = [r["result"] for r in rows] if engine == "native" else [wasm_bench_answer(r["result"]) for r in rows]
        return {"scenario": label, "repeat": rep, "batch_id": bid, "started": started, "finished": finished, "wall_s": max(finished - started, 1e-9), "settings": settings, "per_worker": per_worker, "answer": jobs.combine("benchmark", results), "valid": True}

    def _run(self, ident: str, spec: BenchmarkSpec, workers: list[dict], event: threading.Event) -> None:
        runs, warnings = [], []
        try:
            scenarios = []
            for scenario in spec.scenarios:
                scenarios.extend([w["name"] for w in workers] if scenario == "each" else [scenario])
            scenarios = list(dict.fromkeys(scenarios))
            tops = [int(_TOP.match(s)[1]) for s in scenarios if _TOP.match(s)]
            selected = workers if "all" in scenarios or tops else [w for w in workers if w["name"] in scenarios]
            units = spec.units
            speed: dict[str, float] = {}
            shared = {w["name"]: w["info"].get("speed_units_per_s") for w in selected}
            if all(isinstance(v, (int, float)) and 0 < v < 1e6 for v in shared.values()) and spec.workload == "hash_chain":
                speed = {name: 1 / v for name, v in shared.items()}  # helpers that share their own speed score
            elif units is None or tops:
                for w in selected:  # seconds per unit, alone, natively
                    speed[w["name"]] = self._submit(ident, spec, 2, w["name"], -1, [w], event)["wall_s"] / 2
            if units is None:
                units = calibrated_units(list(speed.values()), {"quick": 30, "standard": 300, "long": 900}[spec.size])
            ranked = sorted(workers, key=lambda w: speed.get(w["name"], math.inf))
            groups = {s: (workers if s == "all" else ranked[:int(_TOP.match(s)[1])] if _TOP.match(s)
                          else [w for w in workers if w["name"] == s]) for s in scenarios}
            schedule = [(scenario, engine, rep) for rep in range(1, spec.repeats + 1) for scenario in scenarios
                        for engine in spec.engines]
            if spec.order == "shuffled":
                random.Random(spec.seed).shuffle(schedule)
            for scenario, engine, rep in schedule:
                targets = groups[scenario]
                self._progress(ident, phase="settling", scenario=scenario, repeat=rep,
                               completed_runs=len(runs), total_runs=len(schedule))
                before = {w["name"]: w["metrics"].get("cpu_percent") for w in self.store.list_workers()}
                warnings.extend(self._settle(targets, event))
                current = {w["id"]: w for w in self.store.list_workers()}
                targets = [current[w["id"]] for w in targets]
                warnings.extend(f"{w['name']} is on battery power." for w in targets if w["metrics"].get("on_ac_power") is False)
                run = self._submit(ident, spec, units, scenario, rep, targets, event, engine)
                for name, settings in run["settings"].items():
                    settings["foreground_cpu_before"] = before.get(name)
                run["valid"] = not runs or run["answer"] == runs[0]["answer"]
                self.store.add_benchmark_run(ident, run)
                runs.append(run)
                self.store.update_benchmark(ident, "running", derive(runs, units) | {"warnings": list(dict.fromkeys(warnings))})
            result = derive(runs, units) | {"warnings": list(dict.fromkeys(warnings))}
            if speed:
                result["calibration_units_per_s"] = {name: round(1 / s, 4) for name, s in speed.items() if s}
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


def device_label(worker: dict[str, Any]) -> dict[str, str]:
    """How a participant is shown to everyone: its device name and short ID, never an IP address."""
    from .identity import short_id

    info = worker.get("info") or {}
    device = info.get("device_id", "")
    out = {"device_name": str(info.get("device_name") or worker.get("name", ""))[:40]}
    if isinstance(device, str) and re.fullmatch(r"[0-9a-f]{64}", device):
        out["short_id"] = short_id(device)
    return out


def shared_result(store: Store, worker_name: str) -> dict[str, Any] | None:
    benchmark = store.latest_participant_benchmark(worker_name)
    if benchmark:
        hardware = {}
        for run in benchmark["runs"]:
            for name, setting in run["settings"].items():
                hardware[name] = {"cpu_name": setting.get("cpu_name"), "ram_total_mb": setting.get("ram_total_mb"), "threads": setting.get("limits", {}).get("max_cpu_threads"),
                                  "device_name": setting.get("device_name", name), "short_id": setting.get("short_id", "")}
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
    fields = ["scenario", "wall_s", "min_s", "max_s", "units_per_s", "speedup", "relative_speed", "engine", "wasm_overhead"]
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
