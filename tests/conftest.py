from __future__ import annotations

import socket
import time
from pathlib import Path

import pytest

from network_connector.client import AdminClient, pair_worker
from network_connector.config import ControllerConfig, WorkerConfig
from network_connector.controller import ControllerRuntime
from network_connector.worker import WorkerAgent, load_worker_config


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(predicate, timeout: float = 60, interval: float = 0.3):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    raise AssertionError("condition not met in time")


class Cluster:
    """A real TLS controller on 127.0.0.1 plus helpers to attach real workers."""

    def __init__(self, home: Path, **cfg_overrides) -> None:
        self.home = home
        self.cfg = ControllerConfig(bind_host="127.0.0.1", port=free_port(), **cfg_overrides)
        self.runtime = ControllerRuntime(self.cfg, home / "controller")
        self.runtime.start()
        self.admin = AdminClient("127.0.0.1", self.cfg.port, self.runtime.cert, self.runtime.admin_token)
        self.agents: list[WorkerAgent] = []

    def add_worker(self, name: str = "w1", **limits) -> WorkerAgent:
        wdir = self.home / f"worker-{name}"
        wcfg = WorkerConfig(max_cpu_threads=2, max_concurrent_jobs=2, max_memory_mb=1024, heartbeat_s=0.5, poll_s=0.3,
                            shutdown_grace_s=5)
        for k, v in limits.items():
            setattr(wcfg, k, v)
        code = self.admin.create_pairing()["code"]
        pair_worker("127.0.0.1", self.cfg.port, code, name, wdir, wcfg)
        agent = WorkerAgent(load_worker_config(wdir) if not limits else _apply(load_worker_config(wdir), limits), wdir)
        agent.start_thread()
        self.agents.append(agent)
        return agent

    def close(self) -> None:
        for a in self.agents:
            a.stop()
        self.admin.close()
        self.runtime.stop()
        self.runtime.store.close()


def _apply(cfg: WorkerConfig, limits: dict) -> WorkerConfig:
    for k, v in limits.items():
        setattr(cfg, k, v)
    return cfg


@pytest.fixture
def cluster(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    c = Cluster(tmp_path, lease_seconds=6, worker_offline_after_s=4)
    yield c
    c.close()


# ---------------------------------------------------------------- Wasm sandbox (Compute agent)
@pytest.fixture(scope="session")
def sandbox_runtime(tmp_path_factory):
    """A real sandbox runtime in a temp folder (never the user's). Its grant is removed afterwards."""
    import os
    import shutil
    import sys

    if sys.platform != "win32":
        pytest.skip("the Wasm sandbox is Windows-only")
    from network_connector import sandbox

    base = tmp_path_factory.mktemp("sandbox")
    old = {k: os.environ.get(k) for k in ("NC_HOME", "NC_RUNTIME_HOME")}
    os.environ["NC_HOME"] = str(base / "home")
    os.environ["NC_RUNTIME_HOME"] = str(base / "runtime")
    try:
        home = sandbox.setup_runtime()
        shutil.copyfile(Path(__file__).with_name("escape_probe.py"), home / "escape_probe.py")
        ok, reason = sandbox.available(refresh=True)
        assert ok, reason
        yield home
    finally:
        sandbox.remove_runtime(base / "runtime")
        for key, value in old.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        sandbox.available(refresh=True)
