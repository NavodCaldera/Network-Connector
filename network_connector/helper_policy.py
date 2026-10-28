"""Local owner limits checked before a helper requests more work."""
from __future__ import annotations

import ctypes
import re
import sys
from datetime import datetime

import psutil

from .config import WorkerConfig

_HOURS = re.compile(r"^([01]\d|2[0-3]):([0-5]\d)-([01]\d|2[0-3]):([0-5]\d)$")


def input_idle_seconds() -> float | None:
    if sys.platform != "win32":
        return None

    class LASTINPUTINFO(ctypes.Structure):
        _fields_ = [("cbSize", ctypes.c_uint), ("dwTime", ctypes.c_uint)]

    info = LASTINPUTINFO()
    info.cbSize = ctypes.sizeof(info)
    if not ctypes.windll.user32.GetLastInputInfo(ctypes.byref(info)):
        return None
    ticks = ctypes.windll.kernel32.GetTickCount() & 0xFFFFFFFF
    return ((ticks - info.dwTime) & 0xFFFFFFFF) / 1000


def can_help(cfg: WorkerConfig, now: datetime | None = None) -> tuple[bool, str]:
    """Fail closed for a configured gate whose state cannot be measured."""
    if cfg.requester_device_id and cfg.requester_device_id in cfg.blocked_requesters:
        return False, "requester blocked by this helper"
    if cfg.ac_only:
        battery = psutil.sensors_battery()
        if battery is not None and not battery.power_plugged:
            return False, "waiting for AC power"
    if cfg.idle_min > 0:
        idle = input_idle_seconds()
        if idle is None or idle < cfg.idle_min * 60:
            return False, "waiting until this computer is idle"
    if cfg.allowed_hours:
        match = _HOURS.fullmatch(cfg.allowed_hours)
        if not match:
            return False, "invalid allowed hours setting"
        start = int(match[1]) * 60 + int(match[2])
        end = int(match[3]) * 60 + int(match[4])
        local_now = now or datetime.now()
        minute = local_now.hour * 60 + local_now.minute
        allowed = start <= minute < end if start < end else minute >= start or minute < end
        if start == end or not allowed:
            return False, "outside allowed helping hours"
    return True, ""
