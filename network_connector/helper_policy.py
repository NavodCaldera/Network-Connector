"""Local owner limits checked before a helper requests more work."""
from __future__ import annotations

import ctypes
import re
import sys
from dataclasses import dataclass, field
from datetime import datetime

import psutil

from .config import HELPER_JOIN_MODES, ConfigError, WorkerConfig

_HOURS = re.compile(r"^([01]\d|2[0-3]):([0-5]\d)-([01]\d|2[0-3]):([0-5]\d)$")
_DEVICE = re.compile(r"^[0-9a-f]{64}$")


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


def limit_reason(cfg: WorkerConfig, used_seconds: float, blocked: bool = False,
                 now: datetime | None = None) -> str:
    """Why this helper must not work for its requester right now ("" = it may). Checked before every
    claim and again while jobs run (PLAN §16.2); `used_seconds` includes CPU time of running jobs."""
    if blocked:
        return "requester blocked by this helper"
    allowed, reason = can_help(cfg, now)
    if not allowed:
        return reason
    if cfg.daily_cpu_min > 0 and used_seconds >= cfg.daily_cpu_min * 60:
        return "daily CPU limit reached"
    return ""


@dataclass
class HelperPolicy:
    """The owner's helper settings (AGENT.md §4 contract). Stored as fields of worker.json."""
    accept: str = "auto"  # auto | ask | trusted
    threads: int = 2
    jobs: int = 2
    mem_mb: int = 1024
    ac_only: bool = True
    idle_min: int = 0
    hours: str = ""
    daily_cpu_min: int = 120
    blocked: list[str] = field(default_factory=list)
    stop_now: bool = False
    cpu_rate: int = 0  # % of the whole CPU all jobs together may use; 0 = no cap

    _FIELDS = {"accept": "join_mode", "threads": "max_cpu_threads", "jobs": "max_concurrent_jobs",
               "mem_mb": "max_memory_mb", "ac_only": "ac_only", "idle_min": "idle_min", "hours": "allowed_hours",
               "daily_cpu_min": "daily_cpu_min", "blocked": "blocked_requesters", "stop_now": "limit_stop_now",
               "cpu_rate": "cpu_rate_percent"}

    @classmethod
    def from_config(cls, cfg: WorkerConfig) -> "HelperPolicy":
        return cls(**{name: getattr(cfg, attr) for name, attr in cls._FIELDS.items()})

    def validate(self) -> "HelperPolicy":
        if self.accept not in HELPER_JOIN_MODES:
            raise ConfigError(f"accept must be one of {', '.join(HELPER_JOIN_MODES)}.")
        for name, lo, hi in (("threads", 1, 1024), ("jobs", 1, 256), ("mem_mb", 256, 1_048_576),
                             ("idle_min", 0, 1440), ("daily_cpu_min", 0, 10080), ("cpu_rate", 0, 100)):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, int) or not lo <= value <= hi:
                raise ConfigError(f"{name} must be a whole number between {lo} and {hi}.")
        if self.hours and not _HOURS.fullmatch(self.hours):
            raise ConfigError("hours must be HH:MM-HH:MM (24-hour clock), or blank for any time.")
        if not isinstance(self.blocked, list) or not all(isinstance(b, str) and _DEVICE.fullmatch(b) for b in self.blocked):
            raise ConfigError("blocked must be a list of full device IDs.")
        self.blocked = list(dict.fromkeys(self.blocked))[:1000]
        return self

    def apply(self, cfg: WorkerConfig) -> WorkerConfig:
        self.validate()
        for name, attr in self._FIELDS.items():
            setattr(cfg, attr, getattr(self, name))
        return cfg
