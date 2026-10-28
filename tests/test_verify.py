"""Result checks (PLAN §18): redundancy, majority, reliability scores, auto-block, and the verifier hook.

The helpers here are scripted clients of the real worker API over pinned TLS, so a helper can lie on purpose.
No module runs: the Wasm sandbox is not involved in judging results.
"""
import base64
import json
from types import SimpleNamespace

import httpx
import pytest

from network_connector import verify
from network_connector.client import controller_url, pinned_context
from network_connector.security import hash_token, new_token
from network_connector.store import Store

from conftest import Cluster
from wasm_modules import ECHO, wasm


class FakeHelper:
    def __init__(self, cluster: Cluster, name: str, device: str, honest: bool = True) -> None:
        token = new_token()
        self.id = cluster.runtime.store.add_worker(name, hash_token(token), {"device_id": device, "group_mode": "open"})
        self.device, self.honest, self.name = device, honest, name
        self.http = httpx.Client(base_url=controller_url("127.0.0.1", cluster.cfg.port), timeout=10,
                                 verify=pinned_context(cluster.runtime.cert), headers={"Authorization": f"Bearer {token}"})

    def step(self) -> int:
        """Heartbeat, take one wasm job and answer it. Returns the HTTP status of the claim (401 once dropped)."""
        hb = self.http.post("/worker/heartbeat", json={"metrics": {}, "version": "test"})
        if hb.status_code != 200:
            return hb.status_code
        resp = self.http.post("/worker/claim", json={"max_jobs": 1, "kinds": ["wasm"]})
        for job in resp.json().get("jobs", []):
            data = base64.b64decode(job["params"]["input_b64"])
            out = data if self.honest else b"wrong:" + data
            self.http.post(f"/worker/jobs/{job['id']}/complete", json={
                "attempt": job["attempt"], "duration": 0.1,
                "result": {"stdout_b64": base64.b64encode(out).decode(), "exit_code": 0, "fuel_used": None,
                           "cpu_seconds": 0.01, "stderr_b64": ""}})
        return resp.status_code


@pytest.fixture
def checked(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    c = Cluster(tmp_path, lease_seconds=30, worker_offline_after_s=30, redundancy_percent=100)
    sha = c.admin.upload_module(wasm(ECHO), "echo")["sha256"]
    yield c, sha
    c.close()


def _submit(cluster: Cluster, sha: str, n: int) -> str:
    items = [{"kind": "wasm", "params": {"module_sha256": sha, "input_b64": base64.b64encode(f"chunk {i}".encode()).decode()}}
             for i in range(n)]
    return cluster.admin.submit_batch(items, label="checked", max_attempts=3)["batch_id"]


def _run(helpers, cluster, batch, rounds=200):
    for _ in range(rounds):
        for h in helpers:
            h.step()
        cluster.runtime.store.expire_leases()
        jobs = cluster.admin.jobs(batch_id=batch)
        if all(j["status"] in ("succeeded", "failed", "cancelled") for j in jobs):
            return jobs
    raise AssertionError("batch did not finish")


def test_lying_helper_is_caught_outvoted_and_dropped(checked):
    cluster, sha = checked
    liar = FakeHelper(cluster, "liar", "1" * 64, honest=False)
    honest = [FakeHelper(cluster, f"h{i}", str(i + 2) * 64) for i in range(2)]
    batch = _submit(cluster, sha, 6)
    jobs = _run([liar, *honest], cluster, batch)
    assert all(j["status"] == "succeeded" for j in jobs)
    for j in jobs:  # every answer is the honest one, whoever ran it first
        assert base64.b64decode(cluster.admin.job(j["id"])["result"]["stdout_b64"]) == f"chunk {j['idx']}".encode()
    store = cluster.runtime.store
    liar_device = store.get_device(liar.device)
    assert liar_device["blocked"] and liar_device["score"] <= store.block_score
    assert store.get_worker("liar")["state"] == "revoked"
    assert liar.step() == 401  # its token no longer works
    assert all(store.get_device(h.device)["score"] > 0 for h in honest)
    states = {c["state"] for c in store.checks()}
    assert "majority" in states and "match" in states
    checks = cluster.admin._call("GET", "/admin/checks")
    assert checks["redundancy_percent"] == 100 and checks["totals"]["majority"] >= 2


def test_twin_never_runs_on_the_same_helper_and_matches_count(checked):
    cluster, sha = checked
    a, b = FakeHelper(cluster, "a", "a" * 64), FakeHelper(cluster, "b", "b" * 64)
    batch = _submit(cluster, sha, 1)
    a.step()
    a.step()  # a must not get its own twin
    job = cluster.admin.jobs(batch_id=batch)[0]
    assert job["status"] == "checking"
    b.step()
    job = cluster.admin.jobs(batch_id=batch)[0]
    assert job["status"] == "succeeded"
    store = cluster.runtime.store
    assert store.checks()[0]["state"] == "match" and store.checks()[0]["match"] == 1
    assert store.get_device("a" * 64)["score"] == 1 and store.get_device("b" * 64)["score"] == 1


def test_lone_helper_is_not_held_up_by_checks(checked):
    cluster, sha = checked
    solo = FakeHelper(cluster, "solo", "5" * 64)
    batch = _submit(cluster, sha, 2)
    jobs = _run([solo], cluster, batch, rounds=5)
    assert [j["status"] for j in jobs] == ["succeeded", "succeeded"]


def test_unanswered_twin_is_given_up_and_the_result_kept(checked):
    cluster, sha = checked
    store = cluster.runtime.store
    a, b = FakeHelper(cluster, "a", "a" * 64), FakeHelper(cluster, "b", "b" * 64)
    b.http.post("/worker/heartbeat", json={"metrics": {}, "version": "test"})
    batch = _submit(cluster, sha, 1)
    a.step()
    assert cluster.admin.jobs(batch_id=batch)[0]["status"] == "checking"
    store.check_wait_s = 0
    store.expire_leases()
    assert cluster.admin.jobs(batch_id=batch)[0]["status"] == "succeeded"
    assert store.checks()[0]["state"] == "unchecked"


def test_low_score_helpers_get_one_chunk_at_a_time(tmp_path):
    store = Store(tmp_path / "s.db")
    wid = store.add_worker("w", "hash", {"device_id": "c" * 64, "group_mode": "open"})
    store.create_batch("wasm", None, None, [("wasm", {"i": i}) for i in range(4)], 60, 1, None)
    worker = store.get_worker(wid)
    store.set_trusted("c" * 64, False)
    with store._tx() as c:
        c.execute("update devices set score=-2 where device_id=?", ("c" * 64,))
    assert len(store.claim(worker, 4, 60, ["wasm"])) == 1
    store.close()


# ---------------------------------------------------------------- verifier hook
def test_verifier_frame_round_trips():
    data = verify.frame(b"input", b"output")
    assert verify.unframe(data) == (b"input", b"output")
    with pytest.raises(ValueError):
        verify.unframe(b"\xff" * 8)


def test_verifier_accepts_rejects_and_requeues_for_another_helper(tmp_path):
    store = Store(tmp_path / "s.db")
    store.add_module("f" * 64, b"\0asm", "verifier")
    a = store.add_worker("a", "ha", {"device_id": "a" * 64, "group_mode": "open"})
    b = store.add_worker("b", "hb", {"device_id": "b" * 64, "group_mode": "open"})
    params = {"module_sha256": "e" * 64, "input_b64": base64.b64encode(b"7").decode(), "args": [], "limits": {}}
    store.create_batch("wasm", None, None, [("wasm", params), ("wasm", params)], 60, 2, None, verifier="f" * 64)
    seen = []

    def judge(module, stdin, args, **limits):
        job_input, output = verify.unframe(stdin)
        seen.append((module, job_input, limits["memory_mb"]))
        return SimpleNamespace(ok=True, exit_code=0 if output == b"49" else 1, outcome="exit")

    runner = verify.VerifierRunner(store, run=judge)
    good, bad = store.claim(store.get_worker(a), 1, 60, ["wasm"])[0], store.claim(store.get_worker(b), 1, 60, ["wasm"])[0]
    for job, wid, out in ((good, a, b"49"), (bad, b, b"50")):
        status = store.finish(job["id"], wid, 1, result={"stdout_b64": base64.b64encode(out).decode(), "exit_code": 0})
        assert status == "verifying"
    assert runner.run_once() and runner.run_once() and not runner.run_once()
    assert seen[0] == (b"\0asm", b"7", verify.VERIFIER_MEMORY_MB)
    assert store.get_job(good["id"])["status"] == "succeeded"
    rejected = store.get_job(bad["id"])
    assert rejected["status"] == "queued" and "verifier" in rejected["error"]
    assert store.claim(store.get_worker(b), 1, 60, ["wasm"]) == []  # never back to the helper it rejected
    assert store.claim(store.get_worker(a), 1, 60, ["wasm"])[0]["id"] == bad["id"]
    assert store.get_device("b" * 64)["score"] < 0 < store.get_device("a" * 64)["score"]
    store.close()


def test_verifier_that_cannot_run_fails_the_job_closed(tmp_path):
    from network_connector import sandbox

    store = Store(tmp_path / "s.db")
    store.add_module("f" * 64, b"\0asm", "verifier")
    a = store.add_worker("a", "ha", {})
    params = {"module_sha256": "e" * 64, "input_b64": "", "args": [], "limits": {}}
    store.create_batch("wasm", None, None, [("wasm", params)], 60, 2, None, verifier="f" * 64)
    job = store.claim(store.get_worker(a), 1, 60, ["wasm"])[0]
    store.finish(job["id"], a, 1, result={"stdout_b64": "", "exit_code": 0})

    def unavailable(*_a, **_k):
        raise sandbox.SandboxUnavailable("no sandbox here")

    verify.VerifierRunner(store, run=unavailable).run_once()
    job = store.get_job(job["id"])
    assert job["status"] == "failed" and "could not run" in job["error"] and job["result"] is None
    store.close()


def test_submitting_with_a_verifier_needs_wasm_a_known_module_and_a_sandbox(checked, monkeypatch):
    from network_connector import sandbox
    from network_connector.client import ControllerError

    cluster, sha = checked
    with pytest.raises(ControllerError, match="wasm"):
        cluster.admin.submit("prime_range", {"start": 0, "end": 100}, verifier_sha256=sha)
    item = {"kind": "wasm", "params": {"module_sha256": sha, "input_b64": ""}}
    with pytest.raises(ControllerError, match="Upload"):
        cluster.admin.submit_batch([item], verifier_sha256="0" * 64)
    monkeypatch.setattr(sandbox, "available", lambda refresh=False: (False, "not on this computer"))
    with pytest.raises(ControllerError, match="not on this computer"):
        cluster.admin.submit_batch([item], verifier_sha256=sha)
    monkeypatch.setattr(sandbox, "available", lambda refresh=False: (True, ""))
    batch = cluster.admin.submit_batch([item], verifier_sha256=sha)["batch_id"]
    job = cluster.runtime.store._q("select verifier from jobs where batch_id=?", (batch,))[0]
    assert job["verifier"] == sha
