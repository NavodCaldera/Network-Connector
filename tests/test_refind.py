"""Real pinned TLS tests for listener changes and certificate-bound discovery."""
import time

from conftest import wait_for
from network_connector.client import AdminClient, pair_worker
from network_connector.config import ControllerConfig, WorkerConfig, load_config, save_config
from network_connector.controller import ControllerRuntime
from network_connector.worker import WorkerAgent, load_worker_config


def test_worker_refinds_same_controller_on_second_alias(cluster, monkeypatch):
    from network_connector import wifidirect
    monkeypatch.setattr(wifidirect, "candidate_controllers", lambda: [])
    rt = cluster.runtime
    rt.add_listener("127.0.0.2")
    rt.add_listener("127.0.0.3")
    directory = cluster.home / "refind-worker"
    code = cluster.admin.create_pairing()["code"]
    cfg = pair_worker("127.0.0.2", rt.cfg.port, code, "roaming", directory,
                      WorkerConfig(heartbeat_s=0.3, poll_s=0.3, shutdown_grace_s=1))
    assert cfg.controller_fingerprint == rt.fingerprint
    cfg.known_hosts = ["127.0.0.2", "127.0.0.3"]
    save_config(cfg, directory / "worker.json")
    agent = WorkerAgent(load_worker_config(directory), directory)
    cluster.agents.append(agent)
    agent.start_thread()
    wait_for(lambda: agent.state == "online", timeout=15)
    token = agent.cfg.token
    rt.remove_listener("127.0.0.2")
    wait_for(lambda: agent.cfg.controller_host == "127.0.0.3" and agent.state == "online", timeout=35)
    assert agent.cfg.token == token
    assert load_config(WorkerConfig, directory / "worker.json").controller_host == "127.0.0.3"
    sub = cluster.admin.submit("number_batch", {"start": 1, "count": 100})
    wait_for(lambda: cluster.admin.batch(sub["batch_id"])["done"], timeout=15)
    assert cluster.admin.batch(sub["batch_id"])["counts"] == {"succeeded": 1}
    assert "127.0.0.1" in rt.listener_hosts  # mandatory local administration is never removed


def test_worker_refuses_impostor_during_refind(cluster, monkeypatch, tmp_path):
    from network_connector import wifidirect
    monkeypatch.setattr(wifidirect, "candidate_controllers", lambda: ["127.0.0.3"])
    directory = cluster.home / "impostor-worker"
    rt = cluster.runtime
    rt.add_listener("127.0.0.2")
    code = cluster.admin.create_pairing()["code"]
    cfg = pair_worker("127.0.0.2", rt.cfg.port, code, "victim", directory, WorkerConfig(shutdown_grace_s=1))
    cfg.known_hosts = ["127.0.0.2", "127.0.0.3"]
    save_config(cfg, directory / "worker.json")
    token = cfg.token
    rt.stop()
    fake = ControllerRuntime(ControllerConfig(bind_host="127.0.0.3", port=rt.cfg.port), tmp_path / "impostor")
    try:
        fake.start()
        agent = WorkerAgent(cfg, directory)
        # Exercise the identical retry helper used after three failed connection rounds.
        assert not agent._try_refind()
        assert agent.cfg.controller_host == "127.0.0.2" and agent.cfg.token == token
        assert not fake.store.list_workers()
    finally:
        fake.stop()
        fake.store.close()


def test_old_pairing_fills_identity_from_pinned_file(cluster):
    directory = cluster.home / "old-pairing"
    code = cluster.admin.create_pairing()["code"]
    cfg = pair_worker("127.0.0.1", cluster.cfg.port, code, "old", directory, WorkerConfig())
    cfg.controller_fingerprint, cfg.known_hosts = "", []
    token = cfg.token
    save_config(cfg, directory / "worker.json")
    upgraded = load_worker_config(directory)
    assert upgraded.controller_fingerprint == cluster.runtime.fingerprint
    assert upgraded.known_hosts == ["127.0.0.1"] and upgraded.token == token
