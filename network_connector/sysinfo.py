"""Hardware and live usage information shown on dashboards and in the demo."""
from __future__ import annotations

import platform
import socket
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
