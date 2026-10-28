import json
import sqlite3
import time

import pytest

from network_connector import jobs
from network_connector.benchmark import BenchmarkRunner, calibrated_units, derive, export_benchmark, shared_result
from network_connector.cli import build_parser, main
from network_connector.store import MIGRATIONS, SCHEMA, Store
from conftest import wait_for


@pytest.mark.parametrize("workload", ["hash_chain", "prime_sieve"])
def test_benchmark_answer_independent_of_chunking(workload, monkeypatch):
    monkeypatch.setattr(jobs, "HASH_ROUNDS", 100)
    monkeypatch.setattr(jobs, "SIEVE_UNIT", 1000)
    params = {"workload": workload, "units": 7, "seed": 42}
    whole = jobs.execute("benchmark", params)
    for chunks in (1, 2, 5, 50):
        assert jobs.combine("benchmark", [jobs.execute("benchmark", p) for p in jobs.split("benchmark", params, chunks)]) == whole


def test_benchmark_limits():
    assert max(p["units"] for p in jobs.split("benchmark", {"units": 20000}, 1)) <= 500
    with pytest.raises(jobs.JobError):
        jobs.execute("benchmark", {"units": 501})
    for params in ({"units": 20001}, {"units": 2, "start": 19999}, {"units": True}, {"units": 1, "rounds": 1}):
        with pytest.raises(jobs.JobError):
            jobs.split("benchmark", params, 2)
    assert calibrated_units([2, 4], 30) == 15


def test_old_database_migration_preserves_rows_and_backups(tmp_path):
    path = tmp_path / "state.db"
    with sqlite3.connect(path) as conn:
        conn.executescript(SCHEMA)
        conn.execute("insert into workers(id,name,token_hash,created) values('old','old-worker','hash',1)")
    store = Store(path)
    assert store.get_worker("old")["name"] == "old-worker"
    assert store._q("select value from meta where key='schema_version'")[0][0] == str(max(MIGRATIONS))
    assert path.with_name("state.db.bak-0.1.0").is_file()
    store.close()
    store = Store(path)
    assert len(store.list_workers()) == 1
    store.close()


def test_samples_prune_and_cancel_all_metrics(tmp_path):
    store = Store(tmp_path / "state.db")
    wid = store.add_worker("one", "hash", {})
    store.touch_worker(wid, None, {"cpu_percent": 5, "mem_percent": 10, "rtt_ms": 2}, "test")
    with store._tx() as conn:
        conn.execute("insert into worker_samples values(?,?,?,?,?,?)", (wid, time.time() - 90000, 0, 0, 0, 0))
    store.prune_samples()
    assert len(store.samples(wid, 0, time.time())) == 1
    bid, _ = store.create_batch("number_batch", None, None, [("number_batch", {"start": 1, "count": 2})] * 3, 60, 1, None)
    worker = {"id": wid, "name": "one"}
    job = store.claim(worker, 1, 60)[0]
    assert store.cancel_all() == {"cancelled": 2, "cancelling": 1}
    assert store.finish(job["id"], wid, 2, result={}, cpu_seconds=999) is None
    assert store.finish(job["id"], wid, 1, cancelled=True, duration=1, cpu_seconds=0.8, peak_mem_mb=20) == "cancelled"
    saved = store.get_job(job["id"])
    assert saved["cpu_seconds"] == saved["attempts"][0]["cpu_seconds"] == 0.8
    assert saved["queue_wait_s"] >= 0
    store.close()


def test_release_preserves_attempt_history(tmp_path):
    store = Store(tmp_path / "s.db")
    wid = store.add_worker("one", "hash", {})
    store.create_batch("number_batch", None, None, [("number_batch", {})], 60, 1, None)
    job = store.claim({"id": wid, "name": "one"}, 1, 60)[0]
    assert store.release(job["id"], wid, 1)
    assert store.get_job(job["id"])["attempts"][0]["outcome"] == "released"
    store.close()


def test_benchmark_two_real_workers_api_cli_exports(cluster, monkeypatch, tmp_path, capsys):
    first, second = cluster.add_worker("one"), cluster.add_worker("two")
    wait_for(lambda: all(a.state == "online" for a in (first, second)))
    runner = cluster.runtime.state.benchmarks
    runner.settle_s = 0
    runner.settle_timeout_s = 0
    ident = cluster.admin.start_benchmark({"units": 2, "repeats": 1})["id"]
    item = wait_for(lambda: (b if (b := cluster.admin.benchmark(ident))["status"] != "running" else None), timeout=80)
    assert item["status"] == "finished", item
    assert len(item["runs"]) == 3
    assert item["result"]["answers_identical"]
    assert item["result"]["speedup"] > 0
    assert item["result"]["efficiency"] > 0
    assert all(r["wall_s"] == r["finished"] - r["started"] for r in item["runs"])
    records = cluster.admin.jobs(batch_id=item["runs"][0]["batch_id"])
    assert all(j["cpu_seconds"] > 0 for j in records)
    assert all(j["peak_mem_mb"] > 0 for j in records)
    assert shared_result(cluster.runtime.store, "one")["id"] == ident
    assert shared_result(cluster.runtime.store, "outsider") is None
    assert "scenario,wall_s" in export_benchmark(item, "csv")
    assert "<svg" in export_benchmark(item, "html")
    assert json.loads(export_benchmark(item))["id"] == ident
    monkeypatch.setattr("network_connector.cli._admin", lambda: cluster.admin)
    assert main(["bench", "show", ident]) == 0
    output = tmp_path / "report.csv"
    assert main(["bench", "export", ident, "--format", "csv", "-o", str(output)]) == 0
    assert output.read_text().startswith("scenario,")
    assert main(["bench", "run", "--units", "4", "--repeats", "1", "--wait"]) == 0


def test_benchmark_api_auth_limits_and_privacy(cluster):
    import httpx
    from network_connector.client import pinned_context

    worker = cluster.add_worker("one")
    wait_for(lambda: worker.state == "online")
    base = f"https://127.0.0.1:{cluster.cfg.port}"
    with httpx.Client(base_url=base, verify=pinned_context(cluster.runtime.cert)) as http:
        assert http.get("/worker/benchmarks/latest").status_code == 401
        assert http.post("/admin/benchmarks", json={"units": 1}).status_code == 401
        assert http.post("/admin/jobs/cancel-all").status_code == 401
        admin = {"X-Admin-Token": cluster.runtime.admin_token}
        worker_headers = {"Authorization": f"Bearer {worker.cfg.token}"}
        assert http.post("/admin/benchmarks", headers=worker_headers, json={"units": 1}).status_code == 401
        assert http.post("/admin/benchmarks", headers=admin, json={"units": 20001}).status_code == 422
        assert http.post("/admin/benchmarks", headers=admin, json={"units": 1, "scenarios": ["x" * 100000]}).status_code in (413, 422)
        assert http.get("/worker/benchmarks/latest", headers=worker_headers).json() == {"benchmark": None}
        for metrics in ({"cpu_percent": {}}, {"rtt_ms": -1}, {"on_ac_power": "yes"}):
            assert http.post("/worker/heartbeat", headers=worker_headers, json={"metrics": metrics}).status_code == 422
        ident, _ = cluster.runtime.store.create_benchmark({"units": 1})
        cluster.runtime.store.add_benchmark_run(ident, {"scenario": "one", "repeat": 1, "batch_id": "none", "started": 1, "finished": 2, "wall_s": 1, "settings": {"one": {"cpu_name": "CPU", "ram_total_mb": 1000, "limits": {"max_cpu_threads": 2}, "token": "secret"}}, "per_worker": {"one": {"chunks": 1}}, "answer": {"units": 1}, "valid": True})
        cluster.runtime.store.update_benchmark(ident, "finished", {"scenarios": []})
        shared = http.get("/worker/benchmarks/latest", headers=worker_headers)
        assert shared.status_code == 200
        assert len(shared.content) <= 65536
        assert "secret" not in shared.text
        assert set(shared.json()["benchmark"]["workers"]["one"]) == {"cpu_name", "ram_total_mb", "threads"}
        cluster.runtime.state.admin_hosts = {"192.168.137.1"}
        assert http.post("/admin/benchmarks", headers=admin, json={"units": 1}).status_code == 403


def test_benchmark_cancel_and_restart(cluster):
    worker = cluster.add_worker("one")
    wait_for(lambda: worker.state == "online")
    runner = cluster.runtime.state.benchmarks
    runner.settle_timeout_s = 0
    ident = cluster.admin.start_benchmark({"units": 500, "repeats": 1})["id"]
    wait_for(lambda: cluster.runtime.store.summary()["running"] > 0)
    assert cluster.admin.cancel_benchmark(ident)["cancelling"]
    wait_for(lambda: cluster.admin.benchmark(ident)["status"] == "cancelled")
    wait_for(lambda: cluster.runtime.store.summary()["running"] == 0)
    pending, _ = cluster.runtime.store.create_benchmark({"units": 1})
    replacement = BenchmarkRunner(cluster.runtime.store)
    assert cluster.admin.benchmark(pending)["status"] == "interrupted"
    replacement.stop()


def test_cli_parsing():
    parser = build_parser()
    assert parser.parse_args(["job", "cancel-all", "--yes"]).yes
    assert parser.parse_args(["bench", "run", "--units", "4", "--wait"]).units == 4
    assert parser.parse_args(["worker", "perf"]).fn.__name__ == "cmd_worker_perf"
