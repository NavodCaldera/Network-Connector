"""Keep job processes from freezing the laptop: low priority and a hard memory cap.

On Windows a Job Object enforces the memory limit and also kills the job process if the
worker itself dies. No administrator rights are needed. Elsewhere only priority is applied.
"""
from __future__ import annotations

import logging
import sys

import psutil

log = logging.getLogger("network_connector.limits")

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _PROCESS_SET_QUOTA = 0x0100
    _PROCESS_TERMINATE = 0x0001
    _LIMIT_PROCESS_MEMORY = 0x00000100
    _LIMIT_KILL_ON_JOB_CLOSE = 0x00002000
    _EXTENDED_LIMIT_INFO = 9
    _CPU_RATE_INFO = 15
    _CPU_RATE_ENABLE = 0x1
    _CPU_RATE_HARD_CAP = 0x4

    class _CpuRate(ctypes.Structure):
        _fields_ = [("ControlFlags", wintypes.DWORD), ("CpuRate", wintypes.DWORD)]

    class _IoCounters(ctypes.Structure):
        _fields_ = [(n, ctypes.c_ulonglong) for n in (
            "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
            "ReadTransferCount", "WriteTransferCount", "OtherTransferCount")]

    class _BasicLimit(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_int64),
            ("PerJobUserTimeLimit", ctypes.c_int64),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class _ExtendedLimit(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", _BasicLimit),
            ("IoInfo", _IoCounters),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    _k32.CreateJobObjectW.restype = wintypes.HANDLE
    _k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    _k32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    _k32.OpenProcess.restype = wintypes.HANDLE
    _k32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    _k32.CloseHandle.argtypes = [wintypes.HANDLE]
    _k32.QueryInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p]


def set_low_priority(pid: int) -> None:
    try:
        proc = psutil.Process(pid)
        if sys.platform == "win32":
            proc.nice(psutil.BELOW_NORMAL_PRIORITY_CLASS)
        else:
            proc.nice(10)
    except (psutil.Error, OSError) as exc:
        log.debug("could not lower priority of %s: %s", pid, exc)


def cpu_rate_value(percent: int) -> int:
    """Windows counts the rate in 1/100 of a percent of all CPUs: 1..10000."""
    if isinstance(percent, bool) or not isinstance(percent, int) or not 1 <= percent <= 100:
        raise ValueError("the CPU-rate cap must be a whole percent between 1 and 100")
    return percent * 100


def set_cpu_rate(job: object, percent: int) -> bool:
    """Hard-cap a Job Object's CPU use. False if Windows refused (callers decide whether to refuse the job)."""
    value = cpu_rate_value(percent)
    if sys.platform != "win32" or not job:
        return False
    info = _CpuRate(_CPU_RATE_ENABLE | _CPU_RATE_HARD_CAP, value)
    return bool(_k32.SetInformationJobObject(job, _CPU_RATE_INFO, ctypes.byref(info), ctypes.sizeof(info)))


def limit_memory(pid: int, megabytes: int, cpu_rate_percent: int = 0) -> object | None:
    """Cap a process's memory (and optionally its CPU rate). Returns a handle for release(); None if unavailable."""
    if sys.platform != "win32":
        return None
    job = _k32.CreateJobObjectW(None, None)
    if not job:
        return None
    info = _ExtendedLimit()
    info.BasicLimitInformation.LimitFlags = _LIMIT_PROCESS_MEMORY | _LIMIT_KILL_ON_JOB_CLOSE
    info.ProcessMemoryLimit = megabytes * 1024 * 1024
    ok = _k32.SetInformationJobObject(job, _EXTENDED_LIMIT_INFO, ctypes.byref(info), ctypes.sizeof(info))
    if ok and cpu_rate_percent and not set_cpu_rate(job, cpu_rate_percent):
        log.warning("CPU-rate cap unavailable for pid %s", pid)
    process = _k32.OpenProcess(_PROCESS_SET_QUOTA | _PROCESS_TERMINATE, False, pid)
    assigned = bool(ok and process and _k32.AssignProcessToJobObject(job, process))
    if process:
        _k32.CloseHandle(process)
    if not assigned:
        log.warning("Job Object memory limit unavailable for pid %s; falling back to monitoring", pid)
        _k32.CloseHandle(job)
        return None
    return job


def release(handle: object | None) -> None:
    if handle and sys.platform == "win32":
        _k32.CloseHandle(handle)


def peak_memory_mb(handle: object | None) -> float | None:
    if handle and sys.platform == "win32":
        info = _ExtendedLimit()
        if _k32.QueryInformationJobObject(handle, _EXTENDED_LIMIT_INFO, ctypes.byref(info), ctypes.sizeof(info), None):
            return info.PeakProcessMemoryUsed / 1024 / 1024
    return None
