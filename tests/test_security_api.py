"""Authentication, pairing and input-validation behaviour of the controller API (no network)."""
import secrets

import pytest
from fastapi.testclient import TestClient

from network_connector.config import ConfigError, ControllerConfig, validate_bind_host
from network_connector.controller import ControllerState, PairingManager, create_app
from network_connector.security import (
    generate_self_signed,
    fingerprint_pem_file,
    new_pairing_code,
    normalize_code,
    pairing_proof,
)
from network_connector.store import Store


@pytest.fixture
def env(tmp_path):
    cert, key = tmp_path / "c.crt", tmp_path / "c.key"
    generate_self_signed(cert, key, ["127.0.0.1"])
    cfg = ControllerConfig(bind_host="127.0.0.1")
    state = ControllerState(
        cfg=cfg, store=Store(tmp_path / "s.db"), fingerprint=fingerprint_pem_file(cert),
        admin_token="admin-secret", admin_hosts={"testclient"}, pairing=PairingManager(600),
    )
    client = TestClient(create_app(state))
    yield client, state
    state.store.close()


ADMIN = {"X-Admin-Token": "admin-secret"}


def pair(client, state, name="w1", code=None, fingerprint=None):
    code = code or client.post("/admin/pairing", headers=ADMIN).json()["code"]
    nonce = secrets.token_hex(16)
    return client.post("/pair", json={
        "worker_name": name, "nonce": nonce,
        "proof": pairing_proof(code, "worker", fingerprint or state.fingerprint, nonce)})


def test_pairing_code_format():
    code = new_pairing_code()
    assert len(code) == 14 and code.count("-") == 2
    assert normalize_code(code.lower()) == normalize_code(code)
    assert len({new_pairing_code() for _ in range(200)}) == 200


def test_admin_requires_token_and_local_host(env):
    client, state = env
    assert client.post("/admin/pairing").status_code == 401
    assert client.post("/admin/pairing", headers={"X-Admin-Token": "wrong"}).status_code == 401
    state.admin_hosts = {"10.0.0.99"}  # pretend the request came from another computer
    r = client.post("/admin/pairing", headers=ADMIN)
    assert r.status_code == 403 and r.json()["error"]["code"] == "admin_local_only"


def test_pairing_success_issues_token_and_controller_proof(env):
    client, state = env
    code = client.post("/admin/pairing", headers=ADMIN).json()["code"]
    nonce = secrets.token_hex(16)
    r = client.post("/pair", json={"worker_name": "w1", "nonce": nonce,
                                   "proof": pairing_proof(code, "worker", state.fingerprint, nonce)})
    assert r.status_code == 200
    body = r.json()
    assert body["controller_proof"] == pairing_proof(code, "controller", state.fingerprint, nonce)
    assert client.post("/worker/heartbeat", json={}, headers={"Authorization": "Bearer " + body["token"]}).status_code == 200


def test_pairing_code_is_single_use(env):
    client, state = env
    code = client.post("/admin/pairing", headers=ADMIN).json()["code"]
    assert pair(client, state, "a", code).status_code == 200
    r = pair(client, state, "b", code)
    assert r.status_code == 403 and r.json()["error"]["code"] == "pairing_inactive"


def test_wrong_code_locks_after_five_attempts(env):
    client, state = env
    real = client.post("/admin/pairing", headers=ADMIN).json()["code"]
    for _ in range(5):
        assert pair(client, state, code="AAAA-AAAA-AAAA").status_code == 403
    r = pair(client, state, code=real)  # even the right code no longer works
    assert r.status_code == 403 and r.json()["error"]["code"] == "pairing_inactive"


def test_impersonated_controller_certificate_fails_pairing(env):
    client, state = env
    r = pair(client, state, fingerprint="0" * 64)  # worker saw a different certificate than the real one
    assert r.status_code == 403


def test_no_pairing_without_admin_issuing_a_code(env):
    client, state = env
    r = pair(client, state, code="ABCD-EFGH-JKMN")
    assert r.status_code == 403


def test_worker_endpoints_need_valid_token(env):
    client, _ = env
    assert client.post("/worker/claim", json={}).status_code == 401
    assert client.post("/worker/claim", json={}, headers={"Authorization": "Bearer nope"}).status_code == 401


def test_revoked_worker_token_stops_working(env):
    client, state = env
    token = pair(client, state, "w1").json()["token"]
    auth = {"Authorization": "Bearer " + token}
    assert client.post("/worker/claim", json={}, headers=auth).status_code == 200
    assert client.post("/admin/workers/w1/revoke", headers=ADMIN).status_code == 200
    assert client.post("/worker/claim", json={}, headers=auth).status_code == 401


def test_workers_cannot_submit_jobs(env):
    client, state = env
    token = pair(client, state, "w1").json()["token"]
    r = client.post("/admin/jobs", json={"kind": "cpu_burn", "params": {"seconds": 1}},
                    headers={"Authorization": "Bearer " + token})
    assert r.status_code == 401


def test_submit_validates_and_allowlists(env):
    client, _ = env
    bad = client.post("/admin/jobs", json={"kind": "powershell", "params": {"cmd": "calc"}}, headers=ADMIN)
    assert bad.status_code == 422 and bad.json()["error"]["code"] == "invalid_job"
    bad = client.post("/admin/jobs", json={"kind": "number_batch", "params": {"start": "x", "count": 1}}, headers=ADMIN)
    assert bad.status_code == 422
    bad = client.post("/admin/jobs", json={"kind": "number_batch", "params": {"start": 1, "count": 5}, "target": "ghost"},
                      headers=ADMIN)
    assert bad.json()["error"]["code"] == "unknown_target"
    ok = client.post("/admin/jobs", json={"kind": "number_batch", "params": {"start": 1, "count": 50}, "chunks": 5},
                     headers=ADMIN)
    assert ok.status_code == 200 and ok.json()["chunks"] == 5


def test_duplicate_submit_returns_same_batch(env):
    client, _ = env
    body = {"kind": "number_batch", "params": {"start": 1, "count": 5}, "idempotency_key": "k1"}
    a = client.post("/admin/jobs", json=body, headers=ADMIN).json()
    b = client.post("/admin/jobs", json=body, headers=ADMIN).json()
    assert a["batch_id"] == b["batch_id"] and a["created"] and not b["created"]


def test_oversized_request_is_refused(env):
    client, state = env
    state.cfg.max_request_bytes = 1000
    r = client.post("/admin/jobs", content=b"x" * 5000, headers=ADMIN | {"content-type": "application/json"})
    assert r.status_code == 413


def test_error_responses_are_structured(env):
    client, _ = env
    r = client.post("/pair", json={"worker_name": "bad name!", "nonce": "zz", "proof": "x"})
    assert r.status_code == 422 and set(r.json()["error"]) == {"code", "message"}


@pytest.mark.parametrize("host", ["0.0.0.0", "::", "8.8.8.8", "example.com"])
def test_bind_host_rejects_public_and_wildcard(host):
    with pytest.raises(ConfigError):
        validate_bind_host(host)


@pytest.mark.parametrize("host", ["192.168.1.4", "10.1.2.3", "127.0.0.1", "172.16.5.5"])
def test_bind_host_accepts_private(host):
    assert validate_bind_host(host) == host


@pytest.mark.parametrize("host", ["192.168.137.1", "169.254.10.1"])
def test_bind_host_accepts_direct_and_link_local(host):
    assert validate_bind_host(host) == host


def test_direct_endpoints_are_admin_only_and_validate_input(env):
    from unittest.mock import Mock
    client, state = env
    state.direct_runtime = runtime = Mock()
    runtime.direct_status.return_value = {"running": False, "mode": "none"}
    runtime.start_direct_group.return_value = {"running": True, "mode": "A"}
    for method, path, body in [("get", "/admin/wifidirect", None),
                              ("post", "/admin/wifidirect", {}),
                              ("post", "/admin/wifidirect/stop", None),
                              ("post", "/admin/wifidirect/listen", {"enabled": True})]:
        kwargs = {"json": body} if body is not None else {}
        assert getattr(client, method)(path, **kwargs).status_code == 401
    assert client.get("/admin/wifidirect", headers=ADMIN).json()["mode"] == "none"
    assert client.post("/admin/wifidirect", headers=ADMIN, json={"ssid": "x" * 33}).status_code == 422
    assert client.post("/admin/wifidirect", headers=ADMIN, json={"passphrase": "short"}).status_code == 422
    assert client.post("/admin/wifidirect", headers=ADMIN, json={"ssid": "DIRECT-NC-1234"}).status_code == 200
    runtime.start_direct_group.assert_called_once_with("DIRECT-NC-1234", None, True)
    state.admin_hosts = {"127.0.0.1", "192.168.137.1"}
    assert client.post("/admin/wifidirect", headers=ADMIN, json={}).status_code == 403


def test_direct_status_failure_is_structured(env):
    from unittest.mock import Mock
    from network_connector.wifidirect import DirectLinkError
    client, state = env
    state.direct_runtime = Mock()
    state.direct_runtime.direct_status.side_effect = DirectLinkError("Windows networking command timed out.")
    response = client.get("/admin/wifidirect", headers=ADMIN)
    assert response.status_code == 503
    assert response.json() == {"error": {"code": "direct_status_unavailable", "message": "Windows networking command timed out."}}


def test_link_is_classified_from_request_source_not_worker_claim(env):
    client, state = env
    worker = pair(client, state).json()
    auth = {"Authorization": "Bearer " + worker["token"]}
    # TestClient supplies a named source, so this is LAN even if the worker claims otherwise.
    reply = client.post("/worker/heartbeat", headers=auth, json={"metrics": {"link": "wifi-direct"}})
    assert reply.json()["link"] == "lan"
    assert client.get("/admin/workers", headers=ADMIN).json()["workers"][0]["link"] == "lan"


def test_direct_peer_cannot_admin_even_on_direct_listener(env):
    from fastapi.testclient import TestClient
    client, state = env
    state.admin_hosts = {"127.0.0.1", "::1", "192.168.137.1"}
    peer = TestClient(create_app(state), client=("192.168.137.22", 12345))
    with peer:
        assert peer.get("/admin/summary", headers=ADMIN).status_code == 403
        assert peer.get("/admin/wifidirect", headers=ADMIN).status_code == 403
