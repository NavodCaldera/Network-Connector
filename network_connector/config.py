"""Configuration: JSON files in the data folder, optionally overridden by NC_* environment variables."""
from __future__ import annotations

import ipaddress
import json
import logging
import logging.handlers
import os
import socket
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path
from typing import Any, TypeVar

import psutil

from .paths import data_home, logs_dir


class ConfigError(ValueError):
    """A configuration value is unsafe or invalid."""


@dataclass
class ControllerConfig:
    bind_host: str = ""  # empty = auto-detect the private LAN address on first start
    bind_hosts: list[str] = field(default_factory=list)
    port: int = 8765
    lease_seconds: int = 60  # a job returns to the queue if its worker is silent this long
    default_timeout_s: int = 600
    default_max_attempts: int = 2
    worker_offline_after_s: int = 20
    pairing_ttl_s: int = 600
    max_request_bytes: int = 24 * 1024 * 1024


@dataclass
class WorkerConfig:
    controller_host: str = ""
    controller_port: int = 8765
    name: str = ""
    worker_id: str = ""
    token: str = ""  # secret: this file is protected and lives outside Git
    controller_fingerprint: str = ""
    known_hosts: list[str] = field(default_factory=list)
    max_cpu_threads: int = 2
    max_concurrent_jobs: int = 2
    max_memory_mb: int = 1024  # per job
    idle_timeout_minutes: int = 0  # 0 = never stop because of idleness
    heartbeat_s: float = 5.0
    poll_s: float = 2.0
    shutdown_grace_s: float = 20.0
    low_priority: bool = True


T = TypeVar("T")


def _cast(value: str, current: Any) -> Any:
    if isinstance(current, list):
        parsed = json.loads(value)
        if not isinstance(parsed, list) or not all(isinstance(v, str) for v in parsed):
            raise ConfigError("Address lists must be JSON arrays of IP address strings.")
        return parsed
    if isinstance(current, bool):
        return value.strip().lower() in {"1", "true", "yes", "on"}
    if isinstance(current, int):
        return int(value)
    if isinstance(current, float):
        return float(value)
    return value


def load_dotenv() -> None:
    """Minimal .env reader (KEY=VALUE). Real environment variables always win."""
    for candidate in (Path.cwd() / ".env", data_home() / ".env"):
        if not candidate.is_file():
            continue
        for line in candidate.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, _, value = line.partition("=")
            os.environ.setdefault(key.strip(), os.path.expandvars(value.strip().strip('"')))


def load_config(cls: type[T], path: Path) -> T:
    cfg = cls()
    if path.is_file():
        raw = json.loads(path.read_text(encoding="utf-8"))
        known = {f.name for f in fields(cls)}  # type: ignore[arg-type]
        for key, value in raw.items():
            if key in known:
                setattr(cfg, key, value)
    for f in fields(cls):  # type: ignore[arg-type]
        env = os.environ.get("NC_" + f.name.upper())
        if env is not None:
            setattr(cfg, f.name, _cast(env, getattr(cfg, f.name)))
    return cfg


def save_config(cfg: Any, path: Path) -> None:
    from .security import protect_path

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(cfg), indent=2), encoding="utf-8")
    protect_path(path)


def validate_bind_host(host: str) -> str:
    """Refuse wildcard and public addresses: the controller is LAN-only."""
    if not isinstance(host, str):
        raise ConfigError("bind_host must be an explicit IP address string.")
    try:
        ip = ipaddress.ip_address(host)
    except ValueError as exc:
        raise ConfigError(f"bind_host must be an IP address, got {host!r}") from exc
    if ip.is_unspecified:
        raise ConfigError(
            "Refusing to bind to 0.0.0.0/:: (all interfaces). Use your private LAN IP, e.g. 192.168.1.4."
        )
    if not (ip.is_private or ip.is_loopback or ip.is_link_local):
        raise ConfigError(f"{host} is a public address; Network Connector only runs on private LAN addresses.")
    return str(ip)


def list_private_ips(labelled: bool = False) -> list[str]:
    found: list[str] = []
    from .wifidirect import direct_interfaces

    try:
        direct = {i.ip: i for i in direct_interfaces()}
    except RuntimeError:
        direct = {}
    for name, addrs in psutil.net_if_addrs().items():
        for addr in addrs:
            if addr.family == socket.AF_INET:
                ip = ipaddress.ip_address(addr.address)
                if ip.is_private and not ip.is_loopback and (not ip.is_link_local or addr.address in direct):
                    label = "Wi-Fi Direct" if addr.address in direct else name
                    value = f"{addr.address} — {label}" if labelled else addr.address
                    if value not in found:
                        found.append(value)
    return found


def detect_lan_ip() -> str:
    """Pick the private IP used for LAN traffic. Sends no packets."""
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
            s.connect(("10.255.255.255", 1))
            ip = s.getsockname()[0]
        if ipaddress.ip_address(ip).is_private:
            return ip
    except OSError:
        pass
    ips = list_private_ips()
    return ips[0] if ips else "127.0.0.1"


def default_worker_limits() -> dict[str, int]:
    logical = psutil.cpu_count(logical=True) or 1
    total_mb = int(psutil.virtual_memory().total / 1024 / 1024)
    jobs = max(1, logical - 1)  # leave one thread free so the laptop stays usable
    return {
        "max_cpu_threads": jobs,
        "max_concurrent_jobs": jobs,
        "max_memory_mb": max(256, int(total_mb * 0.6 / jobs)),
    }


def setup_logging(name: str, console: bool = True) -> logging.Logger:
    logger = logging.getLogger("network_connector")
    if getattr(logger, "_nc_configured", False):
        return logger
    logger.setLevel(logging.INFO)
    fmt = logging.Formatter("%(asctime)s %(levelname)-7s %(name)s: %(message)s")
    file_handler = logging.handlers.RotatingFileHandler(
        logs_dir() / f"{name}.log", maxBytes=1_000_000, backupCount=3, encoding="utf-8"
    )
    file_handler.setFormatter(fmt)
    logger.addHandler(file_handler)
    if console:
        stream = logging.StreamHandler()
        stream.setFormatter(fmt)
        logger.addHandler(stream)
    logger._nc_configured = True  # type: ignore[attr-defined]
    return logger
