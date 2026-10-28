"""Open-group joining without codes (PLAN §16.1, §22.2): signatures, policies, blocking, leave, closing, Verify."""
import base64
import json
import os
import secrets
import subprocess
import sys
from pathlib import Path

import psutil
import pytest
from fastapi.testclient import TestClient

from conftest import wait_for
from network_connector import identity
from network_connector.client import ControllerError, JoinPending, join_requester, leave_requester, verify_requester
from network_connector.config import ControllerConfig, WorkerConfig, load_config, save_config
from network_connector.controller import (
    ControllerRuntime,
    ControllerState,
    PairingManager,
    create_app,
    joined_worker_name,
    worker_group_mode,
)
from network_connector.security import pairing_proof
from network_connector.store import Store
from network_connector.worker import WorkerAgent, load_worker_config

ADMIN = {"X-Admin-Token": "admin-secret"}


@pytest.fixture(scope="module")
def ids(tmp_path_factory):
    base = tmp_path_factory.mktemp("join-ids")
    return {k: identity.load_or_create(base / k) for k in ("requester", "helper", "other")}


@pytest.fixture
def env(tmp_path, ids):
    req = ids["requester"]
    cfg = ControllerConfig(bind_host="127.0.0.1")
    state = ControllerState(
        cfg=cfg, store=Store(tmp_path / "s.db"), fingerprint=req.device_id, admin_token="admin-secret",
        admin_hosts={"testclient"}, pairing=PairingManager(600), identity=req,
    )
    app = create_app(state)
    yield TestClient(app), state, app
    state.store.close()


def hello(client):
    return client.post("/join/hello", json={})


def join(client, state, helper, *, signer=None, challenge=None, requester_id=None, name=None, cert=None):
    challenge = challenge or hello(client).json()["challenge"]
    message = identity.join_message(challenge, requester_id or state.fingerprint, helper.device_id)
    signature = (signer or helper).sign(message)
    return client.post("/join", json={
        "challenge": challenge, "name": name or helper.name, "device_cert": cert or helper.cert_pem,
        "signature": base64.b64encode(signature).decode(), "info": {"cpu_name": "test"}})


def bearer(token):
    return {"Authorization": f"Bearer {token}"}


def heartbeat(client, token, info=None):
    return client.post("/worker/heartbeat", headers=bearer(token), json={"info": info, "metrics": {}})


def test_anyone_joins_without_code_and_server_sets_group(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    first = hello(client).json()
    assert first["requester_device_id"] == state.fingerprint and first["open"] and first["policy"] == "anyone"
    r = join(client, state, helper, challenge=first["challenge"])
    assert r.status_code == 200, r.text
    body = r.json()
    assert body["group_mode"] == "open" and body["worker_name"] == joined_worker_name(helper.name, helper.device_id)
    # The helper can't upgrade itself to a closed (trusted) group through its heartbeat info.
    assert heartbeat(client, body["token"], {"group_mode": "closed", "device_id": "ff" * 32}).status_code == 200
    worker = state.store.get_worker(body["worker_id"])
    info = json.loads(worker["info"])
    assert info["device_id"] == helper.device_id and worker_group_mode(worker) == "open"
    assert state.store.get_device(helper.device_id)["name"] == helper.name
    assert state.store.worker_by_token(body["token"]) is None  # only the hash is stored
    again = join(client, state, helper).json()  # coming back joins again in one step, same worker
    assert again["worker_id"] == body["worker_id"]
    assert heartbeat(client, body["token"]).status_code == 401 and heartbeat(client, again["token"]).status_code == 200


def test_paired_workers_stay_closed(env):
    client, state, _ = env
    code = client.post("/admin/pairing", headers=ADMIN).json()["code"]
    nonce = secrets.token_hex(16)
    r = client.post("/pair", json={"worker_name": "i3", "nonce": nonce,
                                   "proof": pairing_proof(code, "worker", state.fingerprint, nonce)})
    assert r.status_code == 200
    assert worker_group_mode(state.store.get_worker("i3")) == "closed"


def test_wrong_signatures_are_refused(env, ids):
    client, state, _ = env
    helper, other = ids["helper"], ids["other"]
    for kwargs in ({"signer": other}, {"requester_id": other.device_id}, {"cert": other.cert_pem}):
        r = join(client, state, helper, **kwargs)
        assert r.status_code == 403 and r.json()["error"]["code"] == "bad_signature", kwargs
    assert state.store.list_workers() == []


def test_challenges_are_single_use_and_bound_to_the_source(env, ids):
    client, state, app = env
    helper = ids["helper"]
    challenge = hello(client).json()["challenge"]
    assert join(client, state, helper, challenge=challenge).status_code == 200
    assert join(client, state, helper, challenge=challenge).json()["error"]["code"] == "bad_challenge"
    other_source = TestClient(app, client=("192.168.1.77", 5555))
    challenge = hello(other_source).json()["challenge"]
    assert join(client, state, helper, challenge=challenge).json()["error"]["code"] == "bad_challenge"
    assert join(client, state, helper, challenge="A" * 43).json()["error"]["code"] == "bad_challenge"


def test_blocked_device_is_refused_and_its_token_revoked(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    token = join(client, state, helper).json()["token"]
    r = client.post(f"/admin/devices/{helper.short_id}/block", headers=ADMIN)
    assert r.status_code == 200 and r.json()["device_id"] == helper.device_id
    assert heartbeat(client, token).status_code == 401
    r = join(client, state, helper)
    assert r.status_code == 403 and r.json()["error"]["code"] == "device_blocked"
    client.post(f"/admin/devices/{helper.device_id}/unblock", headers=ADMIN)
    assert join(client, state, helper).status_code == 200


def test_require_code_turns_join_off_but_pairing_works(env, ids):
    client, state, _ = env
    assert client.put("/admin/policy", headers=ADMIN, json={"join_policy": "code"}).json()["group_mode"] == "closed"
    assert hello(client).json()["error"]["code"] == "join_disabled"
    r = client.post("/join", json={"challenge": "A" * 43, "name": "x", "device_cert": ids["helper"].cert_pem,
                                   "signature": "AAAAAAAA"})
    assert r.status_code == 403 and r.json()["error"]["code"] == "join_disabled"
    code = client.post("/admin/pairing", headers=ADMIN).json()["code"]
    nonce = secrets.token_hex(16)
    assert client.post("/pair", json={"worker_name": "i3", "nonce": nonce,
                                      "proof": pairing_proof(code, "worker", state.fingerprint, nonce)}).status_code == 200


def test_trusted_only_policy(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    client.put("/admin/policy", headers=ADMIN, json={"join_policy": "trusted"})
    r = join(client, state, helper)
    assert r.status_code == 403 and r.json()["error"]["code"] == "not_trusted"
    assert client.post(f"/admin/devices/{helper.device_id}/trust", headers=ADMIN).status_code == 200
    assert join(client, state, helper).status_code == 200


def test_ask_me_policy_waits_for_accept(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    client.put("/admin/policy", headers=ADMIN, json={"join_policy": "ask"})
    r = join(client, state, helper)
    assert r.status_code == 202 and r.json()["status"] == "pending"
    requests = client.get("/admin/devices", headers=ADMIN).json()["requests"]
    assert [q["device_id"] for q in requests] == [helper.device_id]
    assert client.post(f"/admin/devices/{helper.short_id}/accept", headers=ADMIN).status_code == 200
    assert join(client, state, helper).status_code == 200
    assert join(client, state, helper).status_code == 202  # an approval is used once
    client.post(f"/admin/devices/{helper.device_id}/decline", headers=ADMIN)
    assert join(client, state, helper).status_code == 202
    assert client.post(f"/admin/devices/{ids['other'].device_id}/accept", headers=ADMIN).status_code == 404


def test_joined_name_never_takes_over_a_paired_worker(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    state.store.add_worker(joined_worker_name(helper.name, helper.device_id), "paired-hash", {})
    r = join(client, state, helper)
    assert r.status_code == 409 and r.json()["error"]["code"] == "name_taken"
    assert state.store.worker_by_token("paired-hash") is not None


def test_join_is_rate_limited_and_size_capped(env, ids):
    client, state, _ = env
    codes = [hello(client).status_code for _ in range(35)]
    assert codes.count(200) == 30 and codes[-1] == 429
    r = client.post("/join", content=b"{" + b" " * 40000 + b"}", headers={"content-type": "application/json"})
    assert r.status_code == 413


def test_leave_revokes_token_and_requeues_jobs(env, ids):
    client, state, _ = env
    body = join(client, state, ids["helper"]).json()
    client.post("/admin/jobs", headers=ADMIN, json={"kind": "number_batch", "params": {"start": 1, "count": 5}})
    assert len(client.post("/worker/claim", headers=bearer(body["token"]), json={"max_jobs": 1}).json()["jobs"]) == 1
    assert client.post("/worker/leave", headers=bearer(body["token"]), json={}).json() == {"left": True}
    assert heartbeat(client, body["token"]).status_code == 401
    assert state.store.summary()["queued"] == 1


def test_closing_stops_work_and_joins(env, ids):
    client, state, _ = env
    body = join(client, state, ids["helper"]).json()
    client.post("/admin/jobs", headers=ADMIN, json={"kind": "number_batch", "params": {"start": 1, "count": 5}})
    assert client.post("/admin/close", headers=ADMIN, json={"closing": True}).json()["closing"]
    assert heartbeat(client, body["token"]).json()["closing"] is True
    assert client.post("/worker/claim", headers=bearer(body["token"]), json={"max_jobs": 1}).json()["jobs"] == []
    assert join(client, state, ids["other"]).json()["error"]["code"] == "requester_closing"
    client.post("/admin/close", headers=ADMIN, json={"closing": False})
    assert heartbeat(client, body["token"]).json()["closing"] is False


def test_verify_commit_then_reveal_numbers_match(env, ids):
    client, state, _ = env
    helper = ids["helper"]
    token = join(client, state, helper).json()["token"]
    start = client.post("/worker/verify/start", headers=bearer(token), json={}).json()
    mine = identity.new_nonce()
    theirs = client.post("/worker/verify/finish", headers=bearer(token), json={"session": start["session"], "nonce": mine}).json()["nonce"]
    assert identity.check_commitment(start["commitment"], theirs, helper.device_id, state.fingerprint)
    code = identity.verify_code(helper.device_id, state.fingerprint, mine, theirs)
    shown = client.get("/admin/devices", headers=ADMIN).json()["verifications"]
    assert [(v["device_id"], v["code"]) for v in shown] == [(helper.device_id, code)]
    again = client.post("/worker/verify/finish", headers=bearer(token), json={"session": start["session"], "nonce": mine})
    assert again.status_code == 404  # a session reveals its nonce once


def test_join_admin_routes_are_admin_only(env):
    client, state, _ = env
    for method, path in (("get", "/admin/devices"), ("get", "/admin/policy"), ("get", "/admin/identity"),
                         ("post", "/admin/devices/" + "ab" * 32 + "/trust")):
        assert getattr(client, method)(path).status_code == 401
    state.admin_hosts = {"10.0.0.9"}
    assert client.get("/admin/devices", headers=ADMIN).json()["error"]["code"] == "admin_local_only"
    state.admin_hosts = {"testclient"}
    assert client.put("/admin/policy", headers=ADMIN, json={"join_policy": "everyone"}).status_code == 422
    assert client.post("/admin/devices/not-an-id/trust", headers=ADMIN).status_code == 422
    assert client.post("/admin/devices/" + "ab" * 32 + "/explode", headers=ADMIN).status_code == 404


def test_legacy_controller_config_stays_closed(tmp_path):
    home = tmp_path / "controller"
    home.mkdir()
    (home / "controller.json").write_text(json.dumps({"bind_host": "127.0.0.1", "port": 8765}), encoding="utf-8")
    rt = ControllerRuntime(home=home)
    try:
        assert rt.cfg.join_policy == "code" and not rt.cfg.announce
        assert json.loads((home / "controller.json").read_text())["join_policy"] == "code"
    finally:
        rt.store.close()
    rt = ControllerRuntime(home=home)  # the saved choice sticks
    try:
        assert rt.cfg.join_policy == "code"
    finally:
        rt.store.close()


# ------------------------------------------------------------------ real TLS: one-click join, run, close, leave
def _helper_cfg() -> WorkerConfig:
    return WorkerConfig(max_cpu_threads=1, max_concurrent_jobs=1, max_memory_mb=512, heartbeat_s=0.3, poll_s=0.3,
                        shutdown_grace_s=3)


def _other_device(cluster):
    # The cluster's controller folder is adopted by identity.current(); a helper is another computer.
    return identity.load_or_create(cluster.home / "helper-pc")


def test_helper_joins_runs_job_and_leaves_when_closing(cluster):
    rt = cluster.runtime
    assert identity.current().device_id == rt.fingerprint  # same PC: the controller cert is the device identity
    helper = _other_device(cluster)
    with pytest.raises(ControllerError) as wrong:
        join_requester("127.0.0.1", rt.cfg.port, "00" * 32, helper, None, _helper_cfg())
    assert wrong.value.code == "wrong_requester" and rt.store.list_devices() == []  # nothing was sent
    cfg = join_requester("127.0.0.1", rt.cfg.port, rt.fingerprint, helper, None, _helper_cfg())
    from network_connector.client import joined_dir

    wdir = joined_dir(rt.fingerprint)
    assert cfg.group_mode == "open" and cfg.controller_fingerprint == rt.fingerprint
    assert load_worker_config(wdir).token == cfg.token
    assert verify_requester(cfg, wdir, helper) == rt.state.join.verifications()[0]["code"]
    agent = WorkerAgent(load_worker_config(wdir), wdir)
    cluster.agents.append(agent)
    agent.start_thread()
    sub = cluster.admin.submit("number_batch", {"start": 1, "count": 50})
    wait_for(lambda: cluster.admin.batch(sub["batch_id"])["done"], timeout=20)
    assert cluster.admin.batch(sub["batch_id"])["by_worker"].keys() == {cfg.name}
    rt.state.closing = True
    wait_for(lambda: agent.state == "left", timeout=20)
    assert load_config(WorkerConfig, wdir / "worker.json").token == ""
    assert cluster.admin.workers()[0]["state"] == "revoked"


def test_paired_worker_does_not_leave_on_closing(cluster):
    agent = cluster.add_worker("paired")
    wait_for(lambda: agent.state == "online", timeout=15)
    cluster.runtime.state.closing = True
    wait_for(lambda: agent.remote_closing, timeout=10)
    import time

    time.sleep(1)
    assert agent.state not in ("left", "stopped") and agent.cfg.token
    cluster.runtime.state.closing = False


def test_ask_policy_join_waits_then_succeeds(cluster):
    rt = cluster.runtime
    cluster.admin._call("PUT", "/admin/policy", json={"join_policy": "ask"})
    helper = _other_device(cluster)
    with pytest.raises(JoinPending):
        join_requester("127.0.0.1", rt.cfg.port, rt.fingerprint, helper, None, _helper_cfg())
    cluster.admin._call("POST", f"/admin/devices/{helper.device_id}/accept")
    cfg = join_requester("127.0.0.1", rt.cfg.port, rt.fingerprint, helper, None, _helper_cfg())
    from network_connector.client import joined_dir

    leave_requester(cfg, joined_dir(rt.fingerprint))
    assert load_config(WorkerConfig, joined_dir(rt.fingerprint) / "worker.json").token == ""
    assert cluster.admin.workers()[0]["state"] == "revoked"


def test_announcer_follows_policy_and_persists(cluster):
    rt = cluster.runtime
    assert rt.announcer is not None and rt.announcer.running
    state = rt._announce_state()
    assert state["open"] and state["port"] == rt.cfg.port and state["job_label"] == ""
    assert rt.announcer.send_once() == 0  # loopback-only controller: nothing goes on the network
    policy = cluster.admin._call("PUT", "/admin/policy", json={"join_policy": "code"})
    assert policy["group_mode"] == "closed" and not policy["announcing"] and rt.announcer is None
    assert load_config(ControllerConfig, rt.dir / "controller.json").join_policy == "code"
    assert cluster.admin._call("PUT", "/admin/policy", json={"join_policy": "anyone"})["announcing"]


HELPER_SCRIPT = r"""
import sys, time
from network_connector import discovery, identity
from network_connector.client import join_requester, joined_dir
from network_connector.config import WorkerConfig
from network_connector.worker import WorkerAgent, load_worker_config
port, fingerprint, udp = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
ident = identity.current()
listener = discovery.Listener(ident.device_id, discovery.NearbyList(), port=udp, include_loopback=True,
                              interfaces=lambda: [("Loopback", "127.0.0.1", 8)])
listener.start()
join_requester("127.0.0.1", port, fingerprint, ident, None, WorkerConfig(heartbeat_s=0.3, poll_s=0.3, shutdown_grace_s=1))
agent = WorkerAgent(load_worker_config(joined_dir(fingerprint)), joined_dir(fingerprint))
agent.start_thread()
while agent.state != "online":
    time.sleep(0.1)
print("ready", flush=True)
time.sleep(60)
"""


def test_helper_process_has_no_tcp_listener(cluster, tmp_path):
    from test_discovery import free_udp_port

    script = tmp_path / "helper_proc.py"
    script.write_text(HELPER_SCRIPT, encoding="utf-8")
    env = os.environ | {"NC_HOME": str(tmp_path / "helper-home"), "PYTHONPATH": str(Path(__file__).resolve().parents[1])}
    proc = subprocess.Popen([sys.executable, str(script), str(cluster.cfg.port), cluster.runtime.fingerprint, str(free_udp_port())],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
    try:
        assert proc.stdout.readline().strip() == "ready", proc.stderr.read() if proc.poll() is not None else ""
        root = psutil.Process(proc.pid)  # the venv python.exe is a launcher; the interpreter is its child
        conns = [c for p in [root, *root.children(recursive=True)]
                 for c in (p.net_connections if hasattr(p, "net_connections") else p.connections)(kind="inet")]
        listening = [c for c in conns if c.type == 1 and c.status == psutil.CONN_LISTEN]
        assert listening == []  # helpers open no TCP listening port
        udp = [c for c in conns if c.type == 2]
        assert udp and all(c.laddr.ip not in ("0.0.0.0", "::") for c in udp)  # discovery is bound per address
    finally:
        proc.kill()
        proc.wait(timeout=10)
