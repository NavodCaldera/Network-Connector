"""The `wasm` job kind: validation, modules (upload, hash-checked download, cache), group modes,
end-to-end runs on a test Cluster, Stop/re-queue, and stale-attempt protection."""
from __future__ import annotations

import base64
import hashlib
import sys
from pathlib import Path

import httpx
import pytest

from conftest import wait_for
from network_connector import jobs, modules, sandbox
from network_connector.client import AdminClient, ControllerError, controller_url, pair_worker, pinned_context
from network_connector.config import WorkerConfig
from network_connector.worker import WorkerAgent
from wasm_modules import ECHO, LOOP, PATH_OPEN, wasm

PRIMES = Path(__file__).resolve().parent.parent / "examples" / "wasm" / "primes"
SHA = "ab" * 32
windows_only = pytest.mark.skipif(sys.platform != "win32", reason="the Wasm sandbox is Windows-only")


# ------------------------------------------------------------------ validation (§17.1)
def test_wasm_params_are_validated_and_defaults_filled():
    clean = jobs.validate("wasm", {"module_sha256": SHA})
    assert clean == {"module_sha256": SHA, "input_b64": "", "args": [], "limits": {"memory_mb": 256, "seconds": 60}}
    clean = jobs.validate("wasm", {"module_sha256": SHA, "input_b64": base64.b64encode(b"x").decode(), "args": ["-n", "3"],
                                   "limits": {"memory_mb": 64, "seconds": 5, "fuel": 1000}})
    assert clean["limits"] == {"memory_mb": 64, "seconds": 5, "fuel": 1000}


@pytest.mark.parametrize("params", [
    {},
    {"module_sha256": "AB" * 32},
    {"module_sha256": SHA, "path": "C:/x"},
    {"module_sha256": SHA, "input_b64": "not base64!"},
    {"module_sha256": SHA, "input_b64": "A" * ((jobs.WASM_MAX_INPUT_BYTES + 2) // 3 * 4 + 4)},
    {"module_sha256": SHA, "args": ["a"] * 17},
    {"module_sha256": SHA, "args": ["x" * 257]},
    {"module_sha256": SHA, "args": ["bad\x00arg"]},
    {"module_sha256": SHA, "args": [1]},
    {"module_sha256": SHA, "limits": {"memory_mb": 5000}},
    {"module_sha256": SHA, "limits": {"seconds": 3601}},
    {"module_sha256": SHA, "limits": {"seconds": 0}},
    {"module_sha256": SHA, "limits": {"fuel": 0}},
    {"module_sha256": SHA, "limits": {"cpu": 1}},
])
def test_bad_wasm_params_are_refused(params):
    with pytest.raises(jobs.JobError):
        jobs.validate("wasm", params)


def test_wasm_never_runs_in_the_native_runner_and_is_not_split():
    with pytest.raises(jobs.JobError, match="sandbox"):
        jobs.execute("wasm", {"module_sha256": SHA})
    with pytest.raises(jobs.JobError, match="one job per chunk"):
        jobs.split("wasm", {"module_sha256": SHA}, 4)
    assert "wasm" not in jobs.REGISTRY  # the native allowlist is unchanged and closed


def test_group_modes():
    assert jobs.allowed_kinds("open") == {"wasm"}
    assert jobs.allowed_kinds("closed") == jobs.ALL_KINDS
    with pytest.raises(jobs.JobError):
        jobs.check_group_kind("open", "prime_range")
    jobs.check_group_kind("open", "wasm")
    with pytest.raises(jobs.JobError):
        jobs.allowed_kinds("half-open")


# ------------------------------------------------------------------ modules and the cache (§17.4)
def test_example_module_is_the_compiled_wat_and_only_uses_allowed_imports():
    import wasmtime

    built = (PRIMES / "primes.wasm").read_bytes()
    assert built == wasmtime.wat2wasm((PRIMES / "primes.wat").read_text(encoding="utf-8"))
    assert modules.check(built) == hashlib.sha256(built).hexdigest()
    assert {n for _, n, _ in modules.imports(built)} == {"fd_read", "fd_write", "proc_exit"}


def test_cache_is_hash_checked_and_least_recently_used(tmp_path):
    cache = modules.ModuleCache(tmp_path / "m", max_bytes=600)
    blobs = [wasm(ECHO) + bytes([0, 3, 1, 0, i]) for i in range(3)]  # a custom section makes each one unique
    shas = [modules.sha256_hex(b) for b in blobs]
    cache.put(shas[0], blobs[0])
    cache.put(shas[1], blobs[1])
    assert cache.get(shas[0]) == blobs[0]  # 0 is now the most recently used
    cache.put(shas[2], blobs[2])  # over 600 bytes: the least recently used (1) goes
    assert cache.get(shas[1]) is None and cache.get(shas[0]) == blobs[0] and cache.get(shas[2]) == blobs[2]
    (tmp_path / "m" / f"{shas[0]}.wasm").write_bytes(b"tampered")
    assert cache.get(shas[0]) is None and not (tmp_path / "m" / f"{shas[0]}.wasm").exists()
    with pytest.raises(modules.ModuleError):
        cache.get("../../etc")
    with pytest.raises(modules.ModuleError, match="does not offer"):
        cache.put(modules.sha256_hex(wasm(PATH_OPEN)), wasm(PATH_OPEN))


# ------------------------------------------------------------------ controller API
def _upload(cluster, data: bytes) -> dict:
    return cluster.admin.upload_module(data, "test")


def _raw_worker(cluster, name: str) -> httpx.Client:
    """A paired worker driven by hand (no agent), for protocol-level checks."""
    wdir = cluster.home / f"raw-{name}"
    cfg = pair_worker("127.0.0.1", cluster.cfg.port, cluster.admin.create_pairing()["code"], name, wdir, WorkerConfig())
    return httpx.Client(base_url=controller_url("127.0.0.1", cluster.cfg.port), verify=pinned_context(wdir / "controller.crt"),
                        headers={"Authorization": f"Bearer {cfg.token}"}, timeout=20)


def test_module_upload_checks_and_listing(cluster):
    good = (PRIMES / "primes.wasm").read_bytes()
    res = _upload(cluster, good)
    assert res == {"sha256": hashlib.sha256(good).hexdigest(), "size": len(good), "created": True}
    assert not _upload(cluster, good)["created"]
    assert [m["sha256"] for m in cluster.admin.modules()] == [res["sha256"]]
    for bad, code in ((b"\0asm\1\0\0\0\x01\x05\x01", "invalid_module"), (wasm(PATH_OPEN), "invalid_module"),
                      (b"MZ not wasm", "invalid_module")):
        with pytest.raises(ControllerError) as err:
            _upload(cluster, bad)
        assert err.value.code == code
    with pytest.raises(ControllerError) as err:  # over 10 MB
        _upload(cluster, wasm(ECHO) + b"\0" * modules.MAX_MODULE_BYTES)
    assert err.value.status in (413, 422)
    with pytest.raises(ControllerError) as err:
        cluster.admin.submit_batch([{"kind": "wasm", "params": {"module_sha256": "cd" * 32}}])
    assert err.value.code == "unknown_module"
    with pytest.raises(ControllerError) as err:
        cluster.admin.submit("wasm", {"module_sha256": res["sha256"]}, chunks=3)
    assert err.value.code == "invalid_job"


def test_module_upload_is_admin_only(cluster):
    stranger = AdminClient("127.0.0.1", cluster.cfg.port, cluster.runtime.cert, "wrong-token")
    with pytest.raises(ControllerError) as err:
        stranger.upload_module(wasm(ECHO))
    assert err.value.status == 401
    stranger.close()
    worker = _raw_worker(cluster, "raw")
    assert worker.post("/admin/modules", json={"module_b64": base64.b64encode(wasm(ECHO)).decode()}).status_code == 401
    worker.close()


def test_worker_module_download_needs_a_running_job_that_uses_it(cluster):
    sha = _upload(cluster, wasm(ECHO))["sha256"]
    other = _upload(cluster, wasm(LOOP))["sha256"]
    raw = _raw_worker(cluster, "raw")
    assert raw.get(f"/worker/modules/{sha}").status_code == 404  # no job yet
    assert raw.get("/worker/modules/..%2F..%2Fstate.db").status_code == 404
    cluster.admin.submit_batch([{"kind": "wasm", "params": {"module_sha256": sha}}])
    assert raw.post("/worker/claim", json={"max_jobs": 1}).json()["jobs"] == []  # old workers never get wasm
    job = raw.post("/worker/claim", json={"max_jobs": 1, "kinds": ["wasm"]}).json()["jobs"][0]
    resp = raw.get(f"/worker/modules/{sha}")
    assert resp.status_code == 200 and hashlib.sha256(resp.content).hexdigest() == sha
    assert raw.get(f"/worker/modules/{other}").status_code == 404  # not used by its job
    assert raw.get("/worker/modules/" + sha, headers={"Authorization": "Bearer nope"}).status_code == 401
    # Stale-attempt protection holds for wasm results too.
    body = {"attempt": job["attempt"] + 1, "result": {"stdout_b64": ""}, "duration": 0.1}
    assert raw.post(f"/worker/jobs/{job['id']}/complete", json=body).status_code == 409
    body["attempt"] = job["attempt"]
    body["result"] = {"stdout_b64": "A" * (jobs.WASM_MAX_RESULT_BYTES + 10)}
    assert raw.post(f"/worker/jobs/{job['id']}/complete", json=body).json()["status"] == "failed"  # result too large
    assert raw.post(f"/worker/jobs/{job['id']}/complete", json=body).status_code == 409  # already final
    raw.close()


def test_open_group_hands_out_wasm_only(cluster):
    sha = _upload(cluster, wasm(ECHO))["sha256"]
    cluster.runtime.state.group_mode = "open"
    raw = _raw_worker(cluster, "raw")
    cluster.admin.submit("prime_range", {"start": 0, "end": 1000})
    assert raw.post("/worker/claim", json={"max_jobs": 4, "kinds": sorted(jobs.ALL_KINDS)}).json()["jobs"] == []
    cluster.admin.submit_batch([{"kind": "wasm", "params": {"module_sha256": sha}}])
    got = raw.post("/worker/claim", json={"max_jobs": 4, "kinds": sorted(jobs.ALL_KINDS)}).json()["jobs"]
    assert [j["kind"] for j in got] == ["wasm"]
    assert raw.post("/worker/heartbeat", json={}).json()["group_mode"] == "open"
    cluster.runtime.state.group_mode_for = lambda worker: "bogus"  # unknown modes count as open
    assert raw.post("/worker/heartbeat", json={}).json()["group_mode"] == "open"
    raw.close()


# ------------------------------------------------------------------ helper-side refusals (no controller needed)
def _agent(tmp_path, monkeypatch, **cfg) -> tuple[WorkerAgent, list]:
    agent = WorkerAgent(WorkerConfig(name="h", max_memory_mb=512, **cfg), tmp_path)
    reports: list = []
    monkeypatch.setattr(agent, "_report", lambda job_id, action, body: reports.append((job_id, action, body)))
    return agent, reports


def test_helper_refuses_native_kinds_in_an_open_group(tmp_path, monkeypatch):
    agent, reports = _agent(tmp_path, monkeypatch)
    agent.group_mode = "open"
    assert agent.claim_kinds() in ([], ["wasm"])
    agent._start_job({"id": "j1", "kind": "prime_range", "attempt": 1, "timeout_s": 60, "params": {"start": 0, "end": 10}})
    assert not agent.running
    assert reports[0][1] == "fail" and "open groups" in reports[0][2]["error"] and reports[0][2]["retryable"] is False
    agent.group_mode = "closed"
    agent._remote_group_mode = "open"  # the requester can make it stricter
    assert "prime_range" not in agent.claim_kinds()


def test_helper_without_a_sandbox_refuses_wasm_clearly(tmp_path, monkeypatch):
    agent, reports = _agent(tmp_path, monkeypatch)
    monkeypatch.setattr(sandbox, "available", lambda refresh=False: (False, "The sandbox is not set up yet."))
    assert "wasm" not in agent.claim_kinds() and "prime_range" in agent.claim_kinds()
    agent._start_job({"id": "j2", "kind": "wasm", "attempt": 1, "timeout_s": 60, "params": {"module_sha256": SHA}})
    assert not agent.running
    assert "can't run wasm jobs: The sandbox is not set up yet." in reports[0][2]["error"]


def test_helper_memory_cap_applies_to_wasm(tmp_path, monkeypatch):
    agent, reports = _agent(tmp_path, monkeypatch)
    monkeypatch.setattr(sandbox, "available", lambda refresh=False: (True, ""))
    agent._start_job({"id": "j3", "kind": "wasm", "attempt": 1, "timeout_s": 60,
                      "params": {"module_sha256": SHA, "limits": {"memory_mb": 1024}}})
    assert "this helper allows 512 MB" in reports[0][2]["error"] and not agent.running


# ------------------------------------------------------------------ end to end on a test Cluster (§22.3 item 4)
def _done(cluster, batch_id):
    b = cluster.admin.batch(batch_id)
    return b if b["done"] else None


@windows_only
def test_wasm_primes_end_to_end_matches_native_prime_range(cluster, sandbox_runtime):
    cluster.add_worker("alpha")
    cluster.add_worker("beta")
    wait_for(lambda: sum(w["status"] == "online" for w in cluster.admin.workers()) == 2)
    sha = cluster.admin.upload_module((PRIMES / "primes.wasm").read_bytes(), "primes")["sha256"]
    ranges = [(i * 250_000, (i + 1) * 250_000) for i in range(8)]
    items = [{"kind": "wasm", "params": {"module_sha256": sha, "input_b64": base64.b64encode(f"{a} {b}".encode()).decode(),
                                         "limits": {"memory_mb": 32, "seconds": 60}}} for a, b in ranges]
    sub = cluster.admin.submit_batch(items)
    batch = wait_for(lambda: _done(cluster, sub["batch_id"]), timeout=120)
    assert batch["counts"] == {"succeeded": 8}
    results = [j["result"] for j in sorted(cluster.admin.jobs(batch_id=sub["batch_id"]), key=lambda j: j["idx"])]
    total = sum(int(base64.b64decode(r["stdout_b64"])) for r in results)
    assert total == jobs.execute("prime_range", {"start": 0, "end": 2_000_000})["primes"] == 148_933
    assert all(r["exit_code"] == 0 and r["fuel_used"] is None and r["cpu_seconds"] >= 0 for r in results)
    job = cluster.admin.job(cluster.admin.jobs(batch_id=sub["batch_id"])[0]["id"])
    assert job["cpu_seconds"] is not None and job["peak_mem_mb"] > 0
    for agent in cluster.agents:  # each helper fetched the module once, hash-checked, into its own cache
        if agent.completed:
            assert agent.module_cache.get(sha) is not None
    assert [n for n in sandbox.leftovers() if n.startswith(f"nc-sandbox-{__import__('os').getpid()}-")] == []


@windows_only
def test_wasm_failures_are_reported_not_retried(cluster, sandbox_runtime):
    cluster.add_worker("alpha")
    sha = cluster.admin.upload_module(wasm(LOOP), "loop")["sha256"]
    sub = cluster.admin.submit_batch([{"kind": "wasm", "params": {"module_sha256": sha, "limits": {"seconds": 5, "fuel": 1000}}}],
                                     max_attempts=3)
    batch = wait_for(lambda: _done(cluster, sub["batch_id"]), timeout=60)
    job = cluster.admin.jobs(batch_id=sub["batch_id"])[0]
    assert batch["counts"] == {"failed": 1} and job["attempt"] == 1 and "OUT_OF_FUEL" in job["error"]


@windows_only
def test_stop_kills_sandboxes_within_a_second_and_requeues(cluster, sandbox_runtime):
    agent = cluster.add_worker("alpha")
    sha = cluster.admin.upload_module(wasm(LOOP), "loop")["sha256"]
    sub = cluster.admin.submit_batch([{"kind": "wasm", "params": {"module_sha256": sha, "limits": {"seconds": 600}}}],
                                     timeout_s=900)
    jid = cluster.admin.jobs(batch_id=sub["batch_id"])[0]["id"]
    wait_for(lambda: any(rj.sandbox is not None and rj.sandbox._running_at for rj in list(agent.running.values())), timeout=60)
    agent.pause()  # the owner's Stop also stops taking work (Link agent's tray does both)
    import time

    t0 = time.monotonic()
    assert sandbox.stop_all() == 1
    wait_for(lambda: not sandbox.active(), timeout=5, interval=0.02)
    assert time.monotonic() - t0 < 1.0
    job = wait_for(lambda: (j := cluster.admin.job(jid))["status"] == "queued" and j, timeout=20)
    assert job["attempt"] == 0 and job["attempts"][0]["outcome"] == "released"
    cluster.admin.cancel_job(jid)


# ------------------------------------------------------------------ CLI
def test_cli_sandbox_status_explains_missing_setup(tmp_path, monkeypatch, capsys):
    from network_connector.cli import main

    monkeypatch.setenv("NC_HOME", str(tmp_path / "home"))
    monkeypatch.setenv("NC_RUNTIME_HOME", str(tmp_path / "runtime"))
    monkeypatch.setattr(sandbox, "_avail_cache", None)
    assert main(["sandbox", "status"]) == 1
    out = capsys.readouterr().out
    assert "Sandbox available: no" in out and ("nc sandbox setup" in out or "Windows" in out)
    assert not (tmp_path / "runtime").exists()  # status never creates or grants anything
    monkeypatch.setattr(sandbox, "_avail_cache", None)


@windows_only
def test_cli_sandbox_run_and_check(sandbox_runtime, capsys):
    from network_connector.cli import main

    sandbox.available(refresh=True)
    assert main(["sandbox", "run", str(PRIMES / "primes.wasm"), "--input-text", "0 1000000"]) == 0
    captured = capsys.readouterr()
    assert captured.out.strip() == "78498" and "[ok]" in captured.err
    assert main(["sandbox", "run", str(PRIMES / "primes.wasm"), "--input-text", "nothing"]) == 2
    assert main(["sandbox", "status", "--check"]) == 0
    assert "Self-check: ok" in capsys.readouterr().out


@windows_only
def test_cli_submit_wasm_end_to_end(cluster, sandbox_runtime, monkeypatch, capsys, tmp_path):
    from network_connector import cli

    cluster.add_worker("alpha")
    monkeypatch.setattr(cli, "_admin", lambda: cluster.admin)
    chunk = tmp_path / "chunk.txt"
    chunk.write_text("1000000 2000000", encoding="utf-8")
    assert cli.main(["job", "submit", "wasm", "--module", str(PRIMES / "primes.wasm"), "--input-text", "0 1000000",
                     "--input", str(chunk), "--memory-mb", "32", "--seconds", "60", "--wait"]) == 0
    out = capsys.readouterr().out
    assert "2 job(s)" in out and "78498" in out and "70435" in out  # pi(1e6) and pi(2e6) - pi(1e6)
    assert cli.main(["module", "list"]) == 0
    assert "primes.wasm" in capsys.readouterr().out


def test_cli_parses_wasm_submit_options():
    from network_connector.cli import build_parser

    a = build_parser().parse_args(["job", "submit", "wasm", "--module", "m.wasm", "--input", "a.txt", "--input", "b.txt",
                                   "--arg=-v", "--memory-mb", "64", "--seconds", "30", "--fuel", "1000"])
    assert (a.kind, a.module, a.inputs, a.wasm_args, a.memory_mb, a.seconds, a.fuel) == (
        "wasm", "m.wasm", ["a.txt", "b.txt"], ["-v"], 64, 30, 1000)
    assert build_parser().parse_args(["module", "list"]).sub == "list"
    assert build_parser().parse_args(["sandbox", "setup", "--remove"]).remove
