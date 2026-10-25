"""Where Network Connector keeps its data. Everything lives OUTSIDE the Git repository."""
from __future__ import annotations

import os
from pathlib import Path


def data_home() -> Path:
    """Per-user data folder. Override with the NC_HOME environment variable."""
    env = os.environ.get("NC_HOME")
    if env:
        path = Path(os.path.expandvars(env))
    elif os.environ.get("LOCALAPPDATA"):
        path = Path(os.environ["LOCALAPPDATA"]) / "NetworkConnector"
    else:
        path = Path.home() / ".network_connector"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _sub(name: str) -> Path:
    path = data_home() / name
    path.mkdir(parents=True, exist_ok=True)
    return path


def controller_dir() -> Path:
    return _sub("controller")


def worker_dir(name: str = "worker") -> Path:
    return _sub(name)


def logs_dir() -> Path:
    return _sub("logs")


def repo_root() -> Path | None:
    """The git checkout this code runs from, or None for a non-git install."""
    root = Path(__file__).resolve().parent.parent
    return root if (root / ".git").exists() else None
