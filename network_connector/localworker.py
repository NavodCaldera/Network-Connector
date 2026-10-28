"""Run a worker on the controller computer too, so the i7 also works at full speed."""
from __future__ import annotations

from pathlib import Path

from .client import AdminClient, pair_worker
from .config import WorkerConfig, default_worker_limits, load_config
from .paths import worker_dir
from .worker import NotPaired, WorkerAgent, load_worker_config

LOCAL_NAME = "local-worker"


def local_worker_dir() -> Path:
    return worker_dir("local-worker")


def ensure_paired(admin: AdminClient, host: str, port: int, name: str = "i7-local") -> WorkerConfig:
    """Pair this computer's own worker, using an admin-issued code (no typing needed)."""
    host = "127.0.0.1"
    wdir = local_worker_dir()
    try:
        cfg = load_worker_config(wdir)
        if cfg.controller_host == host and cfg.controller_port == port:
            return cfg
    except NotPaired:
        pass
    cfg = load_config(WorkerConfig, wdir / "worker.json")
    for key, value in default_worker_limits().items():
        setattr(cfg, key, value)
    code = admin.create_pairing()["code"]
    return pair_worker(host, port, code, name, wdir, cfg)


def make_agent(cfg: WorkerConfig) -> WorkerAgent:
    return WorkerAgent(cfg, local_worker_dir())
