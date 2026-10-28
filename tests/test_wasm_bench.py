"""wasm_bench (PLAN §19): the Part 1 hash_chain as a pinned Wasm module, the Wasm overhead column, top-N scenarios.

The module is run here with wasmtime directly (no sandbox) to prove it computes exactly what the native job does.
"""
import base64
import json
import tempfile
import threading
from pathlib import Path

import httpx
import pytest
from pydantic import ValidationError

from network_connector import jobs, modules
from network_connector.benchmark import (WASM_BENCH_SHA256, BenchmarkSpec, derive, wasm_bench_answer, wasm_bench_input,
                                         wasm_bench_module)
from network_connector.client import controller_url, pinned_context
from network_connector.security import hash_token, new_token

from conftest import Cluster, wait_for


def run_module(data: bytes, stdin: bytes) -> bytes:
    from wasmtime import Engine, Linker, Module, Store, WasiConfig

    engine = Engine()
    store = Store(engine)
    linker = Linker(engine)
    linker.define_wasi()
    with tempfile.TemporaryDirectory() as folder:
        src, out = Path(folder, "in"), Path(folder, "out")
        src.write_bytes(stdin)
        wasi = WasiConfig()
        wasi.stdin_file, wasi.stdout_file = str(src), str(out)
        store.set_wasi(wasi)
        instance = linker.instantiate(store, Module(engine, data))
        try:
            instance.exports(store)["_start"](store)
        except Exception as exc:  # noqa: BLE001 - proc_exit(0) surfaces as an exit trap in some versions
            if "exit status 0" not in str(exc):
                raise
        return out.read_bytes()


def test_built_in_module_is_pinned_allowed_and_matches_the_native_job():
    data = wasm_bench_module()
    assert modules.check(data) == WASM_BENCH_SHA256  # size, hash and import allowlist
    assert Path(__file__).resolve().parents[1].joinpath("examples/wasm/bench/bench.wasm").read_bytes() == data
    chunk = {"workload": "hash_chain", "units": 1, "seed": 7, "start": 3}
    out = run_module(data, base64.b64decode(wasm_bench_input(chunk)))
    answer = wasm_bench_answer({"stdout_b64": base64.b64encode(out).decode(), "exit_code": 0})
    assert answer == jobs.execute("benchmark", chunk)


def test_wasm_answers_are_checked_strictly():
    good = {"units": 1, "digest": "0" * 64}
    assert wasm_bench_answer({"stdout_b64": base64.b64encode(json.dumps(good).encode()).decode(), "exit_code": 0}) == good
    for bad in ({"units": 1}, {"units": "1", "digest": "0" * 64}, {"units": 1, "digest": "x" * 64, "extra": 1}):
        with pytest.raises(RuntimeError):
            wasm_bench_answer({"stdout_b64": base64.b64encode(json.dumps(bad).encode()).decode(), "exit_code": 0})
    with pytest.raises(RuntimeError):
        wasm_bench_answer({"stdout_b64": "!!", "exit_code": 0})


def test_spec_engines_and_scenarios():
    assert BenchmarkSpec(engines=["wasm", "wasm"]).engines == ["wasm"]
    with pytest.raises(ValidationError):
        BenchmarkSpec(engines=["wasm"], workload="prime_sieve")
    with pytest.raises(ValidationError):
        BenchmarkSpec(engines=["docker"])


def test_derive_reports_wasm_overhead_per_scenario():
    def run(name, wall, answer="a"):
        return {"scenario": name, "wall_s": wall, "per_worker": {}, "answer": answer}
    result = derive([run("i7", 10), run("i3", 20), run("all", 7), run("i7@wasm", 11), run("all@wasm", 8.4), run("top1", 10)], 100)
    rows = {r["scenario"]: r for r in result["scenarios"]}
    assert rows["i7@wasm"]["wasm_overhead"] == pytest.approx(1.1)
    assert rows["all@wasm"]["wasm_overhead"] == pytest.approx(1.2)
    assert rows["all"]["wasm_overhead"] is None and rows["all@wasm"]["engine"] == "wasm"
    assert result["wasm_overhead"] == pytest.approx(1.15)
    assert rows["all"]["speedup"] == pytest.approx(10 / 7)
    assert rows["top1"]["speedup"] == pytest.approx(1.0)
    assert result["answers_identical"]
    assert not derive([run("i7", 1, "a"), run("i7@wasm", 1, "b")], 1)["answers_identical"]  # wasm must agree


class ScriptedHelper:
    """Answers native benchmark chunks with the real job code and wasm chunks with the real module (no sandbox)."""

    def __init__(self, cluster, name, device):
        token = new_token()
        cluster.runtime.store.add_worker(name, hash_token(token), {"device_id": device, "device_name": f"PC {name}"})
        self.http = httpx.Client(base_url=controller_url("127.0.0.1", cluster.cfg.port), timeout=30,
                                 verify=pinned_context(cluster.runtime.cert), headers={"Authorization": f"Bearer {token}"})
        self.stop = threading.Event()
        self.module = wasm_bench_module()
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.thread.start()

    def loop(self):
        info = {"sandbox": True, "limits": {"max_cpu_threads": 1, "max_concurrent_jobs": 1}, "cpu_name": "test"}
        while not self.stop.is_set():
            self.http.post("/worker/heartbeat", json={"info": info, "metrics": {"cpu_percent": 1}, "version": "test"})
            for job in self.http.post("/worker/claim", json={"max_jobs": 1, "kinds": ["benchmark", "wasm"]}).json()["jobs"]:
                if job["kind"] == "wasm":
                    assert job["params"]["module_sha256"] == WASM_BENCH_SHA256
                    out = run_module(self.module, base64.b64decode(job["params"]["input_b64"]))
                    result = {"stdout_b64": base64.b64encode(out).decode(), "exit_code": 0, "fuel_used": None,
                              "cpu_seconds": 0.1, "stderr_b64": ""}
                else:
                    result = jobs.execute("benchmark", job["params"])
                self.http.post(f"/worker/jobs/{job['id']}/complete",
                               json={"attempt": job["attempt"], "result": result, "duration": 0.1, "cpu_seconds": 0.1})
            self.stop.wait(0.1)


def test_wasm_and_native_engines_with_top_n_give_identical_answers(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    cluster = Cluster(tmp_path, lease_seconds=60, worker_offline_after_s=30)
    helpers = [ScriptedHelper(cluster, n, d * 64) for n, d in (("one", "1"), ("two", "2"))]
    try:
        runner = cluster.runtime.state.benchmarks
        runner.settle_s, runner.settle_timeout_s = 0, 0
        wait_for(lambda: all(w["info"].get("sandbox") for w in cluster.runtime.store.list_workers()), timeout=10)
        ident = cluster.admin.start_benchmark({"units": 2, "repeats": 1, "scenarios": ["one", "top1", "all"],
                                               "engines": ["native", "wasm"]})["id"]
        item = wait_for(lambda: (b if (b := cluster.admin.benchmark(ident))["status"] != "running" else None), timeout=120)
        assert item["status"] == "finished", item
        result = item["result"]
        names = {r["scenario"] for r in result["scenarios"]}
        assert names == {"one", "top1", "all", "one@wasm", "top1@wasm", "all@wasm"}
        assert result["answers_identical"] and result["wasm_overhead"] > 0
        assert set(result["calibration_units_per_s"]) == {"one", "two"}
        top = next(r for r in item["runs"] if r["scenario"] == "top1")
        assert len(top["settings"]) == 1
        assert {s["short_id"] for r in item["runs"] for s in r["settings"].values()} == {"1111-1111", "2222-2222"}
        assert all("127.0.0.1" not in json.dumps(r["settings"]) for r in item["runs"])
    finally:
        for h in helpers:
            h.stop.set()
            h.thread.join(5)
        cluster.close()


def test_wasm_engine_needs_helpers_with_a_sandbox(cluster, monkeypatch):
    from network_connector import sandbox
    from network_connector.client import ControllerError

    # Every heartbeat re-reads sandbox.available(), so make the sandbox unavailable at its source. (Setting
    # worker._info by hand only worked where there is no sandbox at all, e.g. Linux; on a Windows PC with a
    # working sandbox the next heartbeat put True back.)
    monkeypatch.setattr(sandbox, "available", lambda refresh=False: (False, "The sandbox is not set up yet."))
    worker = cluster.add_worker("plain")
    wait_for(lambda: worker.state == "online")
    wait_for(lambda: any(w["name"] == "plain" and w["info"].get("sandbox") is False for w in cluster.admin.workers()))
    with pytest.raises(ControllerError, match="sandbox"):
        cluster.admin.start_benchmark({"units": 1, "engines": ["wasm"]})
