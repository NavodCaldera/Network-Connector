"""Hardware and live usage information shown on dashboards and in the demo."""
from __future__ import annotations

import json
import platform
import socket
import time
from pathlib import Path
from typing import Any

import psutil


def cpu_name() -> str:
    try:
        import winreg

        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
        ) as key:
            return str(winreg.QueryValueEx(key, "ProcessorNameString")[0]).strip()
    except Exception:  # noqa: BLE001 - non-Windows or locked-down registry
        return platform.processor() or "unknown CPU"


def hardware_info() -> dict[str, Any]:
    return {
        "hostname": socket.gethostname(),
        "os": f"{platform.system()} {platform.release()}",
        "cpu_name": cpu_name(),
        "cores_physical": psutil.cpu_count(logical=False) or 0,
        "cores_logical": psutil.cpu_count(logical=True) or 1,
        "ram_total_mb": int(psutil.virtual_memory().total / 1024 / 1024),
    }


def prime_cpu_counter() -> None:
    """psutil reports 0 on the first call, so call it once at startup."""
    psutil.cpu_percent(interval=None)


def live_metrics() -> dict[str, Any]:
    mem = psutil.virtual_memory()
    try:
        battery = psutil.sensors_battery()
    except (psutil.Error, OSError, NotImplementedError):
        battery = None
    try:
        freq = psutil.cpu_freq()
    except (psutil.Error, OSError, NotImplementedError):
        freq = None
    return {
        "cpu_percent": round(psutil.cpu_percent(interval=None), 1),
        "mem_percent": round(mem.percent, 1),
        "mem_used_mb": int(mem.used / 1024 / 1024),
        "on_ac_power": battery.power_plugged if battery else None,
        "battery_percent": battery.percent if battery else None,
        "cpu_freq_mhz": freq.current if freq else None,
    }


def speed_score(cache: Path, version: str) -> float:
    """This PC's single-thread speed in hash_chain benchmark units per second (PLAN §19).

    Measured once per installed version (about one second of CPU) and kept in `cache`. It is only shared with
    requesters when the owner allows it (WorkerConfig.share_speed_score).
    """
    try:
        saved = json.loads(cache.read_text(encoding="utf-8"))
        if saved.get("version") == version and 0 < float(saved["units_per_s"]) < 1e6:
            return float(saved["units_per_s"])
    except (OSError, ValueError, KeyError, TypeError):
        pass
    from . import jobs

    started = time.perf_counter()
    jobs.execute("benchmark", {"workload": "hash_chain", "units": 1, "seed": 0, "start": 0})
    score = round(1 / max(time.perf_counter() - started, 1e-6), 4)
    try:
        cache.write_text(json.dumps({"version": version, "units_per_s": score}), encoding="utf-8")
    except OSError:
        pass
    return score
