"""The Wasm sandbox on a helper: one fresh AppContainer + Job Object per job, deleted afterwards.

Layers (PLAN §17.2), each meant to hold on its own:
1. wasmtime in sandbox_runner.py: WASI stdin/stdout/stderr only, import allowlist, memory/stack limits,
   optional fuel and one-shot epoch.
2. A no-capability AppContainer: no network, no user files. A new profile for every job.
3. A Job Object: memory cap (killed on the limit message), ActiveProcessLimit=1, kill-on-close, UI limits,
   plus the child-process policy in STARTUPINFOEX.
4. The helper's own wall-clock kill (TerminateJobObject): the real time limit.

The runner and a copy of wasmtime live in their own runtime folder (never in the NetworkConnector data
folder, which holds secrets). That folder alone gets `ALL APPLICATION PACKAGES:(OI)(CI)(RX)`.
If any layer can't be set up, available() is False and callers refuse the job. There is no weaker fallback.

Launch recipe and limits come from the Phase 6 spike, spikes/0.3-sandbox/REPORT.md.
"""
from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import secrets
import shutil
import struct
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

log = logging.getLogger("network_connector.sandbox")

PREFIX = "NC-Sandbox-"
RUNNER_NAME = "nc_sandbox_runner.py"
MANIFEST = "runtime.json"
ALL_APP_PACKAGES = "S-1-15-2-1"
STDOUT_CAP = 1024 * 1024
STDERR_CAP = 64 * 1024
PROTOCOL_CAP = 256 * 1024  # runner's own stderr lines (RESULT carries the module's stderr base64)
RUNTIME_OVERHEAD_MB = 160  # Python + wasmtime + compiled code, on top of the module's memory limit
COMPILE_GRACE_S = 15.0  # time allowed for start-up and compiling before the module starts running
MAPPINGS = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings"
_NAME_RE = re.compile(r"^nc-sandbox-(\d+)-[0-9a-f]{12}$", re.IGNORECASE)
_SAFE_FOLDER_RE = re.compile(r"^nc-sandbox-[a-z0-9-]{1,48}$")  # only these are ever removed by name

EXIT_KILLED = {"memory": 0xE0000009, "timeout": 0xE0000102, "stopped": 0xE0000100, "cancelled": 0xE0000101,
               "stdout_cap": 0xE0000103, "stderr_cap": 0xE0000104, "escape": 0xE0000105}


class SandboxUnavailable(RuntimeError):
    """A sandbox layer can't be set up on this computer. The job must be refused."""


@dataclass
class SandboxResult:
    outcome: str  # ok | exit | trap | invalid | refused | link_error | error | crashed | <kill reason>
    exit_code: int | None = None
    stdout: bytes = b""
    stderr: bytes = b""
    detail: str = ""
    fuel_used: int | None = None
    cpu_seconds: float = 0.0
    peak_mem_mb: float = 0.0
    wall_s: float = 0.0
    process_exit: int | None = None
    escape_signal: bool = False
    profile: str = ""
    timings: dict[str, float] = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        """The module ran to its end (any exit code)."""
        return self.outcome in ("ok", "exit")


# ------------------------------------------------------------------ paths and the runtime folder
def runtime_home() -> Path:
    env = os.environ.get("NC_RUNTIME_HOME")
    if env:
        return Path(os.path.expandvars(env))
    return Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "NetworkConnector-runtime"


def base_python() -> Path:
    """The real interpreter. A venv's python.exe is a launcher that starts a child, which the job forbids."""
    return Path(sys.base_prefix) / "python.exe"


def _runner_source() -> Path:
    return Path(__file__).with_name("sandbox_runner.py")


def _wasmtime_package() -> Path:
    import wasmtime

    return Path(wasmtime.__file__).resolve().parent


def _expected_manifest() -> dict[str, str]:
    import wasmtime

    return {"runner_sha256": hashlib.sha256(_runner_source().read_bytes()).hexdigest(),
            "wasmtime": str(getattr(wasmtime, "__version__", "?"))}


def _inside(child: Path, parent: Path) -> bool:
    try:
        child.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def _check_home(home: Path) -> None:
    from .paths import data_home

    if _inside(home, data_home()) or _inside(data_home(), home):
        raise SandboxUnavailable(
            f"The sandbox runtime folder {home} overlaps the Network Connector data folder, which holds secrets. "
            "Use a separate folder (NC_RUNTIME_HOME).")


def setup_runtime(home: Path | None = None) -> Path:
    """Create the runtime folder (runner + wasmtime copy) and grant AppContainers read/execute on it only."""
    home = home or runtime_home()
    _check_home(home)
    home.mkdir(parents=True, exist_ok=True)
    if not has_grant(home, inherit=True):
        # Granted on the folder before copying, so every file inherits it. Never on any other folder.
        proc = subprocess.run(["icacls", str(home), "/grant", f"*{ALL_APP_PACKAGES}:(OI)(CI)(RX)"],
                              capture_output=True, text=True, timeout=60, check=False)
        if proc.returncode != 0:
            raise SandboxUnavailable(f"Could not grant sandbox read access on {home}: {proc.stdout.strip() or proc.stderr.strip()}")
    try:
        current = json.loads((home / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        current = {}
    _copy_runtime(home, current, _expected_manifest())
    available(refresh=True)
    return home


def _runtime_current(home: Path, manifest: dict[str, str], expected: dict[str, str]) -> bool:
    runner = home / RUNNER_NAME
    return (manifest == expected and runner.is_file() and (home / "site" / "wasmtime").is_dir()
            and hashlib.sha256(runner.read_bytes()).hexdigest() == expected["runner_sha256"])


def _copy_runtime(home: Path, current: dict[str, str], expected: dict[str, str]) -> None:
    """Copy the runner and wasmtime into the (already granted) runtime folder."""
    site = home / "site"
    if current.get("wasmtime") != expected["wasmtime"] or not (site / "wasmtime").is_dir():
        shutil.rmtree(site / "wasmtime", ignore_errors=True)
        shutil.copytree(_wasmtime_package(), site / "wasmtime", ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copyfile(_runner_source(), home / RUNNER_NAME)
    (home / MANIFEST).write_text(json.dumps(expected), encoding="utf-8")


def remove_runtime(home: Path | None = None) -> None:
    """Remove the grant and the runtime folder (`nc sandbox setup --remove`)."""
    home = home or runtime_home()
    if home.is_dir():
        subprocess.run(["icacls", str(home), "/remove:g", f"*{ALL_APP_PACKAGES}"], capture_output=True,
                       timeout=60, check=False)
        shutil.rmtree(home, ignore_errors=True)
    available(refresh=True)


# ------------------------------------------------------------------ Win32
if sys.platform == "win32":
    import ctypes
    import msvcrt
    import winreg
    from ctypes import wintypes

    _k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _adv = ctypes.WinDLL("advapi32", use_last_error=True)
    _uenv = ctypes.WinDLL("userenv", use_last_error=True)
    _ole = ctypes.WinDLL("ole32")
    _HRESULT = ctypes.c_long
    _E_ALREADY_EXISTS = -2147024713

    _ATTR_HANDLE_LIST = 0x20002
    _ATTR_SECURITY_CAPABILITIES = 0x20009
    _ATTR_JOB_LIST = 0x2000D
    _ATTR_CHILD_PROCESS_POLICY = 0x2000E
    _CHILD_PROCESS_RESTRICTED = 0x1
    _EXTENDED_STARTUPINFO_PRESENT = 0x00080000
    _CREATE_UNICODE_ENVIRONMENT = 0x00000400
    _DETACHED_PROCESS = 0x00000008  # no console, so no conhost.exe joins the job
    _BELOW_NORMAL_PRIORITY_CLASS = 0x00004000
    _STARTF_USESTDHANDLES = 0x100
    _LIMIT_ACTIVE_PROCESS = 0x8
    _LIMIT_PROCESS_MEMORY = 0x100
    _LIMIT_DIE_ON_UNHANDLED_EXCEPTION = 0x400
    _LIMIT_KILL_ON_JOB_CLOSE = 0x2000
    _MSG_ACTIVE_PROCESS_LIMIT, _MSG_PROCESS_MEMORY_LIMIT, _MSG_JOB_MEMORY_LIMIT = 3, 9, 10
    _WAIT_TIMEOUT = 0x102

    class _SidAndAttributes(ctypes.Structure):
        _fields_ = [("Sid", ctypes.c_void_p), ("Attributes", wintypes.DWORD)]

    class _SecurityCapabilities(ctypes.Structure):
        _fields_ = [("AppContainerSid", ctypes.c_void_p), ("Capabilities", ctypes.POINTER(_SidAndAttributes)),
                    ("CapabilityCount", wintypes.DWORD), ("Reserved", wintypes.DWORD)]

    class _StartupInfo(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("lpReserved", wintypes.LPWSTR), ("lpDesktop", wintypes.LPWSTR),
                    ("lpTitle", wintypes.LPWSTR)] + [(n, wintypes.DWORD) for n in (
                        "dwX", "dwY", "dwXSize", "dwYSize", "dwXCountChars", "dwYCountChars", "dwFillAttribute",
                        "dwFlags")] + [("wShowWindow", wintypes.WORD), ("cbReserved2", wintypes.WORD),
                                       ("lpReserved2", ctypes.c_void_p), ("hStdInput", wintypes.HANDLE),
                                       ("hStdOutput", wintypes.HANDLE), ("hStdError", wintypes.HANDLE)]

    class _StartupInfoEx(ctypes.Structure):
        _fields_ = [("StartupInfo", _StartupInfo), ("lpAttributeList", ctypes.c_void_p)]

    class _ProcessInformation(ctypes.Structure):
        _fields_ = [("hProcess", wintypes.HANDLE), ("hThread", wintypes.HANDLE),
                    ("dwProcessId", wintypes.DWORD), ("dwThreadId", wintypes.DWORD)]

    class _IoCounters(ctypes.Structure):
        _fields_ = [(n, ctypes.c_ulonglong) for n in ("R", "W", "O", "RT", "WT", "OT")]

    class _BasicLimit(ctypes.Structure):
        _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                    ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                    ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                    ("SchedulingClass", wintypes.DWORD)]

    class _ExtendedLimit(ctypes.Structure):
        _fields_ = [("Basic", _BasicLimit), ("Io", _IoCounters), ("ProcessMemoryLimit", ctypes.c_size_t),
                    ("JobMemoryLimit", ctypes.c_size_t), ("PeakProcessMemoryUsed", ctypes.c_size_t),
                    ("PeakJobMemoryUsed", ctypes.c_size_t)]

    class _Accounting(ctypes.Structure):
        _fields_ = [("TotalUserTime", ctypes.c_int64), ("TotalKernelTime", ctypes.c_int64),
                    ("ThisPeriodTotalUserTime", ctypes.c_int64), ("ThisPeriodTotalKernelTime", ctypes.c_int64),
                    ("TotalPageFaultCount", wintypes.DWORD), ("TotalProcesses", wintypes.DWORD),
                    ("ActiveProcesses", wintypes.DWORD), ("TotalTerminatedProcesses", wintypes.DWORD)]

    class _AssocPort(ctypes.Structure):
        _fields_ = [("CompletionKey", ctypes.c_void_p), ("CompletionPort", wintypes.HANDLE)]

    class _AclSize(ctypes.Structure):
        _fields_ = [("AceCount", wintypes.DWORD), ("AclBytesInUse", wintypes.DWORD), ("AclBytesFree", wintypes.DWORD)]

    class _AceHeader(ctypes.Structure):
        _fields_ = [("AceType", ctypes.c_ubyte), ("AceFlags", ctypes.c_ubyte), ("AceSize", wintypes.WORD)]

    class _AllowedAce(ctypes.Structure):
        _fields_ = [("Header", _AceHeader), ("Mask", wintypes.DWORD), ("SidStart", wintypes.DWORD)]

    _uenv.CreateAppContainerProfile.restype = _HRESULT
    _uenv.CreateAppContainerProfile.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.LPCWSTR,
                                                ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p)]
    _uenv.DeleteAppContainerProfile.restype = _HRESULT
    _uenv.DeleteAppContainerProfile.argtypes = [wintypes.LPCWSTR]
    _uenv.DeriveAppContainerSidFromAppContainerName.restype = _HRESULT
    _uenv.DeriveAppContainerSidFromAppContainerName.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p)]
    _uenv.GetAppContainerFolderPath.restype = _HRESULT
    _uenv.GetAppContainerFolderPath.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.LPWSTR)]
    _adv.FreeSid.argtypes = [ctypes.c_void_p]
    _adv.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
    _adv.ConvertStringSidToSidW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p)]
    _adv.EqualSid.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    _adv.GetNamedSecurityInfoW.restype = wintypes.DWORD
    _adv.GetNamedSecurityInfoW.argtypes = [wintypes.LPCWSTR, ctypes.c_int, wintypes.DWORD, ctypes.c_void_p,
                                           ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p,
                                           ctypes.POINTER(ctypes.c_void_p)]
    _adv.GetAclInformation.argtypes = [ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD, ctypes.c_int]
    _adv.GetAce.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p)]
    _k32.LocalFree.argtypes = [ctypes.c_void_p]
    _ole.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    _k32.InitializeProcThreadAttributeList.argtypes = [ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD,
                                                       ctypes.POINTER(ctypes.c_size_t)]
    _k32.UpdateProcThreadAttribute.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.c_size_t, ctypes.c_void_p,
                                               ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p]
    _k32.DeleteProcThreadAttributeList.argtypes = [ctypes.c_void_p]
    _k32.CreateProcessW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, ctypes.c_void_p, ctypes.c_void_p,
                                    wintypes.BOOL, wintypes.DWORD, ctypes.c_void_p, wintypes.LPCWSTR,
                                    ctypes.POINTER(_StartupInfoEx), ctypes.POINTER(_ProcessInformation)]
    _k32.CreateJobObjectW.restype = wintypes.HANDLE
    _k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    _k32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    _k32.QueryInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
                                               ctypes.c_void_p]
    _k32.TerminateJobObject.argtypes = [wintypes.HANDLE, wintypes.UINT]
    _k32.CreateIoCompletionPort.restype = wintypes.HANDLE
    _k32.CreateIoCompletionPort.argtypes = [wintypes.HANDLE, wintypes.HANDLE, ctypes.c_size_t, wintypes.DWORD]
    _k32.GetQueuedCompletionStatus.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD),
                                               ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_void_p),
                                               wintypes.DWORD]
    _k32.PostQueuedCompletionStatus.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.c_size_t, ctypes.c_void_p]
    _k32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    _k32.WaitForSingleObject.restype = wintypes.DWORD
    _k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
    _k32.CloseHandle.argtypes = [wintypes.HANDLE]


def _check_hr(hr: int, what: str) -> None:
    if hr != 0:
        raise SandboxUnavailable(f"{what} failed (HRESULT {hr & 0xFFFFFFFF:#010x})")


def _sid_string(psid: int) -> str:
    s = wintypes.LPWSTR()
    if not _adv.ConvertSidToStringSidW(psid, ctypes.byref(s)):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return str(s.value)
    finally:
        _k32.LocalFree(s)


def _create_profile(name: str) -> int:
    psid = ctypes.c_void_p()
    hr = _uenv.CreateAppContainerProfile(name, name, "Network Connector sandbox (one job)", None, 0, ctypes.byref(psid))
    if hr == _E_ALREADY_EXISTS:
        _check_hr(_uenv.DeriveAppContainerSidFromAppContainerName(name, ctypes.byref(psid)), "DeriveAppContainerSid")
    else:
        _check_hr(hr, "CreateAppContainerProfile")
    return int(psid.value)


def _folder_path(sid: str) -> str:
    p = wintypes.LPWSTR()
    _check_hr(_uenv.GetAppContainerFolderPath(sid, ctypes.byref(p)), "GetAppContainerFolderPath")
    try:
        return str(p.value)
    finally:
        _ole.CoTaskMemFree(p)


def _delete_profile(name: str) -> int:
    return int(_uenv.DeleteAppContainerProfile(name))


def has_grant(path: Path, inherit: bool = False) -> bool:
    """True if ALL APPLICATION PACKAGES may read and execute `path` (and, with inherit, everything below it)."""
    if sys.platform != "win32" or not path.exists():
        return False
    want = 0x1200A9  # FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
    sd, dacl, sid = ctypes.c_void_p(), ctypes.c_void_p(), ctypes.c_void_p()
    if _adv.GetNamedSecurityInfoW(str(path), 1, 4, None, None, ctypes.byref(dacl), None, ctypes.byref(sd)) != 0:
        return False
    try:
        if not dacl.value or not _adv.ConvertStringSidToSidW(ALL_APP_PACKAGES, ctypes.byref(sid)):
            return False
        info = _AclSize()
        if not _adv.GetAclInformation(dacl, ctypes.byref(info), ctypes.sizeof(info), 2):
            return False
        allowed = False
        for i in range(info.AceCount):
            ace = ctypes.c_void_p()
            if not _adv.GetAce(dacl, i, ctypes.byref(ace)):
                continue
            entry = ctypes.cast(ace, ctypes.POINTER(_AllowedAce)).contents
            if entry.Header.AceType not in (0, 1) or entry.Header.AceFlags & 0x08:  # allow/deny, not inherit-only
                continue
            if not _adv.EqualSid(ctypes.c_void_p(ace.value + 8), sid):
                continue
            if entry.Header.AceType == 1:
                return False  # an explicit deny for app containers
            flags_ok = not inherit or entry.Header.AceFlags & 0x03 == 0x03
            if entry.Mask & want == want and flags_ok:
                allowed = True
        return allowed
    finally:
        if sid.value:
            _k32.LocalFree(sid)
        _k32.LocalFree(sd)


# ------------------------------------------------------------------ availability
_avail_lock = threading.Lock()
_avail_cache: tuple[float, bool, str] | None = None
AVAILABLE_CACHE_S = 30.0


def available(refresh: bool = False) -> tuple[bool, str]:
    """(True, "") if every layer can be set up here; otherwise (False, a reason for the owner)."""
    global _avail_cache
    with _avail_lock:
        if not refresh and _avail_cache and time.monotonic() - _avail_cache[0] < AVAILABLE_CACHE_S:
            return _avail_cache[1], _avail_cache[2]
        ok, reason = _check_available()
        _avail_cache = (time.monotonic(), ok, reason)
        if not ok:
            log.info("sandbox unavailable: %s", reason)
        return ok, reason


def _check_available() -> tuple[bool, str]:
    if sys.platform != "win32":
        return False, "The Wasm sandbox needs Windows (AppContainer and Job Objects)."
    if sys.getwindowsversion().build < 9200:
        return False, "The Wasm sandbox needs Windows 8 or later."
    try:
        expected = _expected_manifest()
    except ImportError:
        return False, "The wasmtime package is not installed. Reinstall Network Connector."
    home = runtime_home()
    try:
        _check_home(home)
    except SandboxUnavailable as exc:
        return False, str(exc)
    from .paths import data_home

    if has_grant(data_home()):
        return False, f"The data folder {data_home()} is readable by app containers; remove that grant, it holds secrets."
    python = base_python()
    if not python.is_file():
        return False, f"The base Python interpreter was not found at {python}."
    if not has_grant(python):
        return False, (f"Python at {python} is not readable from an app container. Install Python for all users "
                       "(Program Files), which already allows this.")
    try:
        manifest = json.loads((home / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False, "The sandbox is not set up yet. Run:  nc sandbox setup"
    if not has_grant(home, inherit=True):
        return False, f"The sandbox runtime folder {home} lacks its read grant. Run:  nc sandbox setup"
    if not _runtime_current(home, manifest, expected):
        try:  # after an update: refresh our own files; the existing grant is reused, nothing new is granted
            _copy_runtime(home, manifest, expected)
        except OSError as exc:
            return False, f"The sandbox runtime is out of date and could not be refreshed ({exc}). Run:  nc sandbox setup"
    name = _new_name()
    try:
        psid = _create_profile(name)
        _adv.FreeSid(psid)
    except (SandboxUnavailable, OSError) as exc:
        return False, f"AppContainer profiles can't be created: {exc}"
    finally:
        _delete_profile(name)
    return True, ""


# ------------------------------------------------------------------ one sandbox per job
_active: set["Sandbox"] = set()
_active_lock = threading.Lock()


def _new_name() -> str:
    return f"{PREFIX}{os.getpid()}-{secrets.token_hex(6)}"


def _attr_list(count: int) -> Any:
    size = ctypes.c_size_t()
    _k32.InitializeProcThreadAttributeList(None, count, 0, ctypes.byref(size))
    buf = (ctypes.c_byte * size.value)()
    if not _k32.InitializeProcThreadAttributeList(buf, count, 0, ctypes.byref(size)):
        raise ctypes.WinError(ctypes.get_last_error())
    return buf


def _update_attr(alist: Any, attr: int, value: Any, size: int) -> None:
    if not _k32.UpdateProcThreadAttribute(alist, 0, attr, value, size, None, None):
        raise SandboxUnavailable(f"UpdateProcThreadAttribute({attr:#x}) failed: {ctypes.WinError(ctypes.get_last_error())}")


class Sandbox:
    """One single-use sandbox for one module run. Create, run() once, and it deletes itself."""

    def __init__(self, module: bytes, stdin: bytes | bytearray = b"", args: list[str] | None = None, *,
                 memory_mb: int = 256, seconds: float = 60, fuel: int | None = None,
                 clean_timeout: bool = False, command: list[str] | None = None,
                 stdout_cap: int = STDOUT_CAP, stderr_cap: int = STDERR_CAP) -> None:
        self.module, self.stdin = module, bytearray(stdin)
        self.args, self.memory_mb, self.seconds, self.fuel = list(args or []), memory_mb, seconds, fuel
        self.clean_timeout, self.stdout_cap, self.stderr_cap = clean_timeout, stdout_cap, stderr_cap
        self.command = command  # tests only: a native program launched in the same container + job
        self.name = _new_name()
        self.pid = 0
        self._lock = threading.Lock()
        self._job: int | None = None
        self._kill_reason = ""
        self._escape = False
        self._running_at: float | None = None
        self._result_line: dict[str, Any] | None = None
        self._out = bytearray()
        self._err_lines: list[bytes] = []
        self._used = False

    # ---- control (thread-safe)
    def kill(self, reason: str) -> None:
        with self._lock:
            if not self._kill_reason:
                self._kill_reason = reason
            if self._job:
                _k32.TerminateJobObject(self._job, EXIT_KILLED.get(reason, 0xE00000FF))

    @property
    def started(self) -> bool:
        return self.pid != 0

    # ---- run
    def run(self) -> SandboxResult:
        if self._used:
            raise RuntimeError("a sandbox is used for one run only")
        self._used = True
        ok, reason = available()
        if not ok:
            raise SandboxUnavailable(reason)
        with _active_lock:
            _active.add(self)
        t0 = time.monotonic()
        psid, port, hprocess, hthread = 0, None, None, None
        threads: list[threading.Thread] = []
        stop_watch = threading.Event()
        timings: dict[str, float] = {}
        try:
            psid = _create_profile(self.name)
            folder = _folder_path(_sid_string(psid))
            job = self._make_job()
            port = _k32.CreateIoCompletionPort(wintypes.HANDLE(-1), None, 0, 1)
            if not port:
                raise SandboxUnavailable("CreateIoCompletionPort failed")
            assoc = _AssocPort(None, port)
            if not _k32.SetInformationJobObject(job, 7, ctypes.byref(assoc), ctypes.sizeof(assoc)):
                raise SandboxUnavailable("Job Object completion port could not be set")
            threads.append(threading.Thread(target=self._watch, args=(port, stop_watch), daemon=True))
            threads[-1].start()
            timings["prepare_ms"] = (time.monotonic() - t0) * 1000
            fds = self._launch(psid, job, folder)
            hprocess, hthread = fds.pop("process"), fds.pop("thread")
            t_launch = time.monotonic()
            for target, arg in ((self._feed, fds["in"]), (self._read_out, fds["out"]), (self._read_err, fds["err"])):
                threads.append(threading.Thread(target=target, args=(arg,), daemon=True))
                threads[-1].start()
            self._wait(hprocess, t_launch)
            timings["process_ms"] = (time.monotonic() - t_launch) * 1000
            code = wintypes.DWORD()
            _k32.GetExitCodeProcess(hprocess, ctypes.byref(code))
            for t in threads[1:]:
                t.join(5)
            peak, cpu = self._job_stats(job)
            return self._result(code.value, peak, cpu, time.monotonic() - t0, timings)
        finally:
            self._cleanup(psid, port, hprocess, hthread, stop_watch, threads)
            with _active_lock:
                _active.discard(self)

    def _make_job(self) -> int:
        job = _k32.CreateJobObjectW(None, None)
        if not job:
            raise SandboxUnavailable("CreateJobObject failed")
        with self._lock:
            self._job = job
        info = _ExtendedLimit()
        info.Basic.LimitFlags = (_LIMIT_KILL_ON_JOB_CLOSE | _LIMIT_DIE_ON_UNHANDLED_EXCEPTION | _LIMIT_PROCESS_MEMORY
                                 | _LIMIT_ACTIVE_PROCESS)
        info.Basic.ActiveProcessLimit = 1
        info.ProcessMemoryLimit = (self.memory_mb + RUNTIME_OVERHEAD_MB) * 1024 * 1024
        if not _k32.SetInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info)):
            raise SandboxUnavailable(f"Job Object limits could not be set: {ctypes.WinError(ctypes.get_last_error())}")
        ui = wintypes.DWORD(0xFF)  # JOB_OBJECT_UILIMIT_ALL: desktop, clipboard, global atoms, handles, exit windows
        if not _k32.SetInformationJobObject(job, 4, ctypes.byref(ui), 4):
            raise SandboxUnavailable("Job Object UI limits could not be set")
        return job

    def _watch(self, port: int, stop: threading.Event) -> None:
        msg, key, ov = wintypes.DWORD(), ctypes.c_size_t(), ctypes.c_void_p()
        while not stop.is_set():
            if not _k32.GetQueuedCompletionStatus(port, ctypes.byref(msg), ctypes.byref(key), ctypes.byref(ov), 100):
                continue
            if msg.value in (_MSG_PROCESS_MEMORY_LIMIT, _MSG_JOB_MEMORY_LIMIT):
                self.kill("memory")
            elif msg.value == _MSG_ACTIVE_PROCESS_LIMIT:
                # The runner never starts processes, so this is an escape attempt.
                self._escape = True
                self.kill("escape")

    def _launch(self, psid: int, job: int, folder: str) -> dict[str, Any]:
        home = runtime_home()
        python = str(base_python())
        cmd = self.command or [python, "-I", "-S", "-B", str(home / RUNNER_NAME), "--site", str(home / "site")]
        env = {"SystemRoot": os.environ.get("SystemRoot", r"C:\Windows"),
               "LOCALAPPDATA": os.environ.get("LOCALAPPDATA", folder),  # required, or CreateProcess fails (203)
               "TEMP": folder, "TMP": folder,
               "PROCESSOR_ARCHITECTURE": os.environ.get("PROCESSOR_ARCHITECTURE", "AMD64")}
        env_block = ctypes.create_unicode_buffer("".join(f"{k}={v}\0" for k, v in env.items()) + "\0")
        in_r, in_w = os.pipe()
        out_r, out_w = os.pipe()
        err_r, err_w = os.pipe()
        child = [msvcrt.get_osfhandle(fd) for fd in (in_r, out_w, err_w)]
        for h in child:
            os.set_handle_inheritable(h, True)
        keep: list[Any] = []
        alist = _attr_list(4)
        try:
            handles = (wintypes.HANDLE * 3)(*child)
            caps = _SecurityCapabilities(psid, None, 0, 0)  # no capabilities at all
            jobs = (wintypes.HANDLE * 1)(job)
            policy = wintypes.DWORD(_CHILD_PROCESS_RESTRICTED)
            keep += [handles, caps, jobs, policy]
            _update_attr(alist, _ATTR_HANDLE_LIST, handles, ctypes.sizeof(handles))
            _update_attr(alist, _ATTR_SECURITY_CAPABILITIES, ctypes.byref(caps), ctypes.sizeof(caps))
            _update_attr(alist, _ATTR_JOB_LIST, jobs, ctypes.sizeof(jobs))
            _update_attr(alist, _ATTR_CHILD_PROCESS_POLICY, ctypes.byref(policy), ctypes.sizeof(policy))
            si = _StartupInfoEx()
            si.StartupInfo.cb = ctypes.sizeof(si)
            si.StartupInfo.dwFlags = _STARTF_USESTDHANDLES
            si.StartupInfo.hStdInput, si.StartupInfo.hStdOutput, si.StartupInfo.hStdError = child
            si.lpAttributeList = ctypes.cast(alist, ctypes.c_void_p)
            pi = _ProcessInformation()
            flags = (_EXTENDED_STARTUPINFO_PRESENT | _CREATE_UNICODE_ENVIRONMENT | _DETACHED_PROCESS
                     | _BELOW_NORMAL_PRIORITY_CLASS)
            cmdline = ctypes.create_unicode_buffer(subprocess.list2cmdline(cmd))
            ok = _k32.CreateProcessW(cmd[0], cmdline, None, None, True, flags, env_block, folder,
                                     ctypes.byref(si), ctypes.byref(pi))
            err = ctypes.get_last_error()
        finally:
            _k32.DeleteProcThreadAttributeList(alist)
            for fd in (in_r, out_w, err_w):
                os.close(fd)
        if not ok:
            for fd in (in_w, out_r, err_r):
                os.close(fd)
            raise SandboxUnavailable(f"The sandbox process could not be started: {ctypes.WinError(err)}")
        self.pid = pi.dwProcessId
        with self._lock:
            if self._kill_reason:  # stopped while starting
                _k32.TerminateJobObject(job, EXIT_KILLED.get(self._kill_reason, 0xE00000FF))
        return {"process": pi.hProcess, "thread": pi.hThread, "in": in_w, "out": out_r, "err": err_r}

    def _frame(self) -> bytes:
        header = json.dumps({
            "v": 1, "module_len": len(self.module), "args": self.args, "memory_mb": self.memory_mb,
            "fuel": self.fuel or 0, "epoch_seconds": self.seconds if self.clean_timeout else 0,
            "stdout_cap": self.stdout_cap, "stderr_cap": self.stderr_cap,
        }).encode()
        return struct.pack("<I", len(header)) + header

    def _feed(self, fd: int) -> None:
        try:
            if self.command is None:
                os.write(fd, self._frame())
                view = memoryview(self.module)
                while view:
                    view = view[os.write(fd, view[:1 << 20]):]
            view = memoryview(self.stdin)
            while view:
                view = view[os.write(fd, view[:1 << 20]):]
            view.release()
        except OSError:
            pass  # the sandbox ended before reading everything
        finally:
            os.close(fd)
            self.stdin[:] = bytes(len(self.stdin))  # wipe the helper's copy of the input
            self.stdin.clear()

    def _read_out(self, fd: int) -> None:
        try:
            while data := os.read(fd, 65536):
                room = self.stdout_cap - len(self._out)
                if len(data) > room:
                    self._out += data[:max(room, 0)]
                    self.kill("stdout_cap")  # the runner caps first; this is the helper's own check
                    break
                self._out += data
        except OSError:
            pass
        finally:
            os.close(fd)

    def _read_err(self, fd: int) -> None:
        size, pending = 0, b""
        try:
            while data := os.read(fd, 65536):
                size += len(data)
                if size > PROTOCOL_CAP:
                    self.kill("stderr_cap")
                    break
                pending += data
                *lines, pending = pending.split(b"\n")
                for line in lines:
                    self._protocol_line(line.rstrip(b"\r"))
            if pending:
                self._protocol_line(pending)
        except OSError:
            pass
        finally:
            os.close(fd)

    def _protocol_line(self, line: bytes) -> None:
        if line == b"STAGE running":
            self._running_at = time.monotonic()
        elif line.startswith(b"RESULT "):
            try:
                parsed = json.loads(line[7:])
                self._result_line = parsed if isinstance(parsed, dict) else None
            except ValueError:
                pass
        elif self.command is not None:
            self._err_lines.append(line)

    def _wait(self, hprocess: int, t_launch: float) -> None:
        start_deadline = t_launch + self.seconds + COMPILE_GRACE_S
        while _k32.WaitForSingleObject(hprocess, 25) == _WAIT_TIMEOUT:
            now = time.monotonic()
            deadline = min(start_deadline, self._running_at + self.seconds) if self._running_at else start_deadline
            if now > deadline:
                self.kill("timeout")
                _k32.WaitForSingleObject(hprocess, 5000)
                return

    def _job_stats(self, job: int) -> tuple[float, float]:
        info, acct = _ExtendedLimit(), _Accounting()
        peak = cpu = 0.0
        if _k32.QueryInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info), None):
            peak = info.PeakProcessMemoryUsed / 1024 / 1024
        if _k32.QueryInformationJobObject(job, 1, ctypes.byref(acct), ctypes.sizeof(acct), None):
            cpu = (acct.TotalUserTime + acct.TotalKernelTime) / 1e7
        return round(peak, 1), round(cpu, 3)

    def _result(self, process_exit: int, peak: float, cpu: float, wall: float, timings: dict[str, float]) -> SandboxResult:
        import base64

        line = self._result_line or {}
        r = SandboxResult(outcome="crashed", stdout=bytes(self._out), process_exit=process_exit, peak_mem_mb=peak,
                          cpu_seconds=cpu, wall_s=round(wall, 3), escape_signal=self._escape, profile=self.name,
                          timings=timings)
        if self._kill_reason:
            r.outcome = self._kill_reason
            r.detail = {"memory": f"used more than the memory limit ({self.memory_mb} MB + runtime)",
                        "timeout": f"ran longer than {self.seconds:g} s", "stopped": "stopped by the helper's owner",
                        "cancelled": "cancelled", "stdout_cap": f"stdout over {self.stdout_cap} bytes",
                        "stderr_cap": "too much protocol output",
                        "escape": "tried to start another process (blocked)"}.get(self._kill_reason, self._kill_reason)
        elif line:
            r.outcome = str(line.get("outcome", "error"))
            r.detail = str(line.get("detail", ""))[:500]
            if r.outcome == "trap" and r.detail == "INTERRUPT":
                r.outcome, r.detail = "timeout", f"ran longer than {self.seconds:g} s"
        elif self.command is not None:
            r.outcome, r.exit_code = "exit", process_exit  # a native test program has no RESULT line
        else:
            r.detail = f"the sandbox process ended without a result (exit code {process_exit:#x})"
        if line.get("exit_code") is not None and r.ok:
            r.exit_code = int(line["exit_code"])
        fuel = line.get("fuel_used")
        r.fuel_used = int(fuel) if isinstance(fuel, int) and not isinstance(fuel, bool) else None
        try:
            r.stderr = base64.b64decode(str(line.get("stderr_b64", "")))[:self.stderr_cap]
        except ValueError:
            r.stderr = b""
        if self.command is not None:
            r.stderr = b"\n".join(self._err_lines)[:self.stderr_cap]
        return r

    def _cleanup(self, psid: int, port: Any, hprocess: Any, hthread: Any, stop_watch: threading.Event,
                 threads: list[threading.Thread]) -> None:
        with self._lock:
            job, self._job = self._job, None
        if job:
            _k32.TerminateJobObject(job, EXIT_KILLED.get(self._kill_reason, 0xE00000FF))
        if hprocess:
            _k32.WaitForSingleObject(hprocess, 5000)
            _k32.CloseHandle(hprocess)
        if hthread:
            _k32.CloseHandle(hthread)
        if job:
            _k32.CloseHandle(job)  # kill-on-close: nothing in the job survives this
        stop_watch.set()
        if port:
            _k32.PostQueuedCompletionStatus(port, 0, 0, None)  # wake the watcher now
        for t in threads:
            if t is not threading.current_thread():
                t.join(2)
        if port:
            _k32.CloseHandle(port)
        if psid:
            _adv.FreeSid(psid)
        self.stdin[:] = bytes(len(self.stdin))
        hr = _delete_profile(self.name)
        folder = Path(os.environ.get("LOCALAPPDATA", "")) / "Packages" / self.name.lower()
        if folder.exists():
            shutil.rmtree(folder, ignore_errors=True)
        if hr != 0 or folder.exists():
            log.warning("sandbox profile %s not fully removed (hr %#x); the start-up sweep will retry", self.name, hr & 0xFFFFFFFF)


def run(module: bytes, stdin: bytes = b"", args: list[str] | None = None, **limits: Any) -> SandboxResult:
    """Run one module in a fresh sandbox and delete it. Raises SandboxUnavailable."""
    return Sandbox(module, stdin, args, **limits).run()


def active() -> list[Sandbox]:
    with _active_lock:
        return list(_active)


def stop_all() -> int:
    """Kill every running sandbox in this process now (the owner's Stop). Returns how many were killed."""
    victims = active()
    for box in victims:
        box.kill("stopped")
    if victims:
        log.info("stopped %d sandbox(es)", len(victims))
    return len(victims)


# ------------------------------------------------------------------ leftovers and the start-up sweep
def _registry_monikers() -> list[str]:
    found: list[str] = []
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, MAPPINGS) as key:
            i = 0
            while True:
                try:
                    sub = winreg.EnumKey(key, i)
                except OSError:
                    break
                i += 1
                try:
                    with winreg.OpenKey(key, sub) as s:
                        moniker = str(winreg.QueryValueEx(s, "Moniker")[0])
                except OSError:
                    continue
                if moniker.lower().startswith(PREFIX.lower()):
                    found.append(moniker)
    except OSError:
        pass
    return found


def leftovers() -> list[str]:
    """Names of every NC sandbox profile or folder that exists now (live ones included)."""
    if sys.platform != "win32":
        return []
    names = {m.lower() for m in _registry_monikers()}
    packages = Path(os.environ.get("LOCALAPPDATA", "")) / "Packages"
    if packages.is_dir():
        names |= {p.name.lower() for p in packages.iterdir() if p.name.lower().startswith(PREFIX.lower())}
    return sorted(names)


def _is_live(name: str, mine: set[str]) -> bool:
    if name in mine:
        return True
    match = _NAME_RE.match(name)
    if not match:
        return False
    pid = int(match.group(1))
    if pid == os.getpid():
        return False
    import psutil

    return psutil.pid_exists(pid)


def sweep() -> int:
    """Delete profiles and folders left by a crash or power loss. Skips sandboxes of running helpers."""
    if sys.platform != "win32":
        return 0
    mine = {box.name.lower() for box in active()}
    removed = 0
    packages = Path(os.environ.get("LOCALAPPDATA", "")) / "Packages"
    for name in leftovers():
        if _is_live(name, mine):
            continue
        _delete_profile(name)
        folder = packages / name
        if _SAFE_FOLDER_RE.match(name) and folder.exists():
            shutil.rmtree(folder, ignore_errors=True)  # Python >= 3.8 removes junctions without following them
        removed += 1
    if removed:
        log.info("sandbox sweep removed %d leftover profile(s)", removed)
    return removed


# A 227-byte WASI command (fd_read/fd_write only) that copies stdin to stdout. Used by self_check().
ECHO_WASM = bytes.fromhex(
    "0061736d01000000010c0260047f7f7f7f017f60000002440216776173695f736e617073686f745f70726576696577310766"
    "645f72656164000016776173695f736e617073686f745f70726576696577310866645f77726974650000030201010503010001"
    "071302066d656d6f72790200065f737461727400020a47014501017f034041004180083602004104418020360200410041004101"
    "410810000d014108280200210020004504400f0b41042000360200410141004101410810011a0c000b0b001e046e616d650107"
    "02000172010177020601020100016e030601020100016c"
)


def self_check() -> SandboxResult:
    """Run a real module end to end in a fresh sandbox. Used by `nc sandbox status --check`."""
    return run(ECHO_WASM, b"sandbox ok\n", memory_mb=16, seconds=10)
