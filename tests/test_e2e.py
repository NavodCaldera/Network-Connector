"""Real TLS controller + real worker processes on localhost."""
import base64
import io
import time

import httpx
import pytest
from PIL import Image

from conftest import Cluster, free_port, wait_for
from network_connector.client import AdminClient, ControllerError, fetch_server_cert, pair_worker, pinned_context
from network_connector.config import ControllerConfig, WorkerConfig
from network_connector.controller import ControllerRuntime
from network_connector.demo import KNOWN_PRIMES_BELOW_1E8, run_demo


def done(cluster, batch_id):
    b = cluster.admin.batch(batch_id)
    return b if b["done"] else None


def test_distributed_prime_count_is_correct_and_attributed(cluster):
    cluster.add_worker("alpha")
    cluster.add_worker("beta")
    wait_for(lambda: sum(w["status"] == "online" for w in cluster.admin.workers()) == 2)
    sub = cluster.admin.submit("prime_range", {"start": 0, "end": 5_000_000}, chunks=12)
    batch = wait_for(lambda: done(cluster, sub["batch_id"]))
    assert batch["counts"] == {"succeeded": 12}
    assert batch["combined"]["primes"] == 348_513  # pi(5,000,000)
    assert sum(v["chunks"] for v in batch["by_worker"].values()) == 12


def test_target_pins_work_to_one_worker(cluster):
    cluster.add_worker("alpha")
    cluster.add_worker("beta")
    wait_for(lambda: sum(w["status"] == "online" for w in cluster.admin.workers()) == 2)
    sub = cluster.admin.submit("number_batch", {"start": 1, "count": 600_000}, chunks=6, target="beta")
    batch = wait_for(lambda: done(cluster, sub["batch_id"]))
    assert list(batch["by_worker"]) == ["beta"]


def test_image_and_file_jobs_run_on_worker(cluster):
    cluster.add_worker("alpha")
    buf = io.BytesIO()
    Image.new("RGB", (200, 100), "red").save(buf, "PNG")
    res = cluster.admin.submit_batch([
        {"kind": "image_resize", "params": {"name": "a.png", "image_b64": base64.b64encode(buf.getvalue()).decode(),
                                            "width": 50, "height": 50}},
        {"kind": "hash_files", "params": {"files": [{"name": "h.txt", "data_b64": base64.b64encode(b"hello").decode()}]}},
    ])
    batch = wait_for(lambda: done(cluster, res["batch_id"]))
    assert batch["counts"] == {"succeeded": 2}
    jobs = {j["kind"]: j for j in cluster.admin.jobs(batch_id=res["batch_id"])}
    assert jobs["image_resize"]["result"]["output_size"] == [50, 25]


def test_timeout_then_retry_then_failure(cluster):
    cluster.add_worker("alpha")
    sub = cluster.admin.submit("cpu_burn", {"seconds": 30}, timeout_s=2, max_attempts=2)
    batch = wait_for(lambda: done(cluster, sub["batch_id"]), timeout=40)
    job = cluster.admin.jobs(batch_id=sub["batch_id"])[0]
    assert job["status"] == "failed" and job["attempt"] == 2 and "timed out" in job["error"]
    assert batch["counts"] == {"failed": 1}


def test_cancel_running_job(cluster):
    cluster.add_worker("alpha")
    sub = cluster.admin.submit("cpu_burn", {"seconds": 60})
    jid = wait_for(lambda: next((j["id"] for j in cluster.admin.jobs(batch_id=sub["batch_id"]) if j["status"] == "running"), None))
    cluster.admin.cancel_job(jid)
    batch = wait_for(lambda: done(cluster, sub["batch_id"]), timeout=20)
    assert batch["counts"] == {"cancelled": 1}
    assert not cluster.agents[0].running  # the process was really killed


def test_pause_stops_new_work_and_resume_continues(cluster):
    cluster.add_worker("alpha")
    wait_for(lambda: cluster.admin.workers()[0]["status"] == "online")
    cluster.admin.worker_action("alpha", "pause")
    wait_for(lambda: cluster.admin.workers()[0]["status"] == "paused")
    sub = cluster.admin.submit("number_batch", {"start": 1, "count": 1000})
    time.sleep(2)
    assert cluster.admin.batch(sub["batch_id"])["counts"] == {"queued": 1}
    cluster.admin.worker_action("alpha", "resume")
    wait_for(lambda: done(cluster, sub["batch_id"]))


def test_concurrency_limit_is_respected(cluster):
    agent = cluster.add_worker("alpha", max_concurrent_jobs=1, max_cpu_threads=4)
    cluster.admin.submit("cpu_burn", {"seconds": 3}, chunks=3)
    peak = 0
    deadline = time.time() + 30
    while time.time() < deadline and cluster.admin.summary()["jobs"]["succeeded"] < 3:
        peak = max(peak, len(agent.running))
        time.sleep(0.1)
    assert peak == 1


def test_revoked_worker_is_stopped(cluster):
    agent = cluster.add_worker("alpha")
    wait_for(lambda: cluster.admin.workers()[0]["status"] == "online")
    cluster.admin.worker_action("alpha", "revoke")
    wait_for(lambda: agent.state == "revoked", timeout=15)


def test_worker_lost_mid_job_is_requeued_to_another_worker(cluster):
    a = cluster.add_worker("alpha", max_concurrent_jobs=1)
    sub = cluster.admin.submit("cpu_burn", {"seconds": 4}, target=None)
    wait_for(lambda: a.running)
    for rj in list(a.running.values()):  # simulate the machine dying: process and agent vanish with no goodbye
        a._kill(rj)
    a.stop_event.set()
    a._thread.join(timeout=10)
    cluster.agents.remove(a)
    cluster.add_worker("beta")
    batch = wait_for(lambda: done(cluster, sub["batch_id"]), timeout=45)
    assert batch["counts"] == {"succeeded": 1}
    assert list(batch["by_worker"]) == ["beta"]


def test_worker_reconnects_after_controller_restart(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    c = Cluster(tmp_path, lease_seconds=6, worker_offline_after_s=4)
    try:
        c.add_worker("alpha")
        sub = c.admin.submit("number_batch", {"start": 1, "count": 100}, chunks=1)
        wait_for(lambda: done(c, sub["batch_id"]))
        port = c.cfg.port
        c.runtime.stop()
        c.runtime.store.close()
        time.sleep(1.5)
        assert c.agents[0].state in ("offline", "online", "connecting")
        c.runtime = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1", port=port, lease_seconds=6, worker_offline_after_s=4),
                                      tmp_path / "controller")
        c.runtime.start()
        c.admin = AdminClient("127.0.0.1", port, c.runtime.cert, c.runtime.admin_token)
        wait_for(lambda: c.admin.workers() and c.admin.workers()[0]["status"] == "online", timeout=40)
        assert c.admin.summary()["jobs"]["succeeded"] == 1  # history survived the restart
        sub2 = c.admin.submit("number_batch", {"start": 1, "count": 100})
        wait_for(lambda: done(c, sub2["batch_id"]))
    finally:
        c.close()


def test_certificate_pinning_blocks_impostor(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    real = Cluster(tmp_path / "real")
    fake = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1", port=free_port()), tmp_path / "fake" / "controller")
    fake.start()
    try:
        wait_for(lambda: real.runtime.running and fake.running)
        # A client pinned to the REAL controller's certificate refuses to talk to the impostor.
        client = httpx.Client(base_url=f"https://127.0.0.1:{fake.cfg.port}", verify=pinned_context(real.runtime.cert))
        with pytest.raises(httpx.HTTPError):
            client.get("/health")
        # Pairing against the impostor with the real controller's code cannot succeed either.
        code = real.admin.create_pairing()["code"]
        with pytest.raises(ControllerError):
            pair_worker("127.0.0.1", fake.cfg.port, code, "victim", tmp_path / "victim", WorkerConfig())
    finally:
        fake.stop()
        real.close()


def test_unpaired_client_cannot_use_worker_api(cluster):
    pem, _ = fetch_server_cert("127.0.0.1", cluster.cfg.port)
    cert = cluster.home / "seen.crt"
    cert.write_text(pem)
    with httpx.Client(base_url=f"https://127.0.0.1:{cluster.cfg.port}", verify=pinned_context(cert)) as c:
        assert c.post("/worker/claim", json={}).status_code == 401
        assert c.post("/admin/jobs", json={"kind": "cpu_burn", "params": {"seconds": 1}}).status_code in (401, 403)


def test_demo_runs_and_verifies_answer(cluster):
    cluster.add_worker("i3-sim")
    cluster.add_worker("i7-sim")
    wait_for(lambda: sum(w["status"] == "online" for w in cluster.admin.workers()) == 2)
    lines: list[str] = []
    result = run_demo(cluster.admin, out=lines.append, chunks=20)
    assert result["ok"] and result["primes"] == KNOWN_PRIMES_BELOW_1E8
    text = "\n".join(lines)
    assert "Connected workers" in text and "Who did the work" in text and "i3-sim" in text


def test_graceful_shutdown_returns_unfinished_job_to_queue(cluster):
    a = cluster.add_worker("alpha", shutdown_grace_s=1)
    sub = cluster.admin.submit("cpu_burn", {"seconds": 60}, max_attempts=1)
    wait_for(lambda: a.running)
    a.stop()
    batch = cluster.admin.batch(sub["batch_id"])
    assert batch["counts"] == {"queued": 1}  # released without consuming its only attempt
