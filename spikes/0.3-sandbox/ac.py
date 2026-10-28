"""Throwaway: launch a process in a fresh no-capability AppContainer + Job Object (prototype of sandbox.py).

Flow per run: CreateAppContainerProfile -> Job Object (memory cap, ActiveProcessLimit, kill-on-close,
UI limits, completion port) -> CreateProcessW with STARTUPINFOEX attributes
(SECURITY_CAPABILITIES, HANDLE_LIST, JOB_LIST, optional CHILD_PROCESS_POLICY / LPAC opt-out)
-> wait -> close job -> DeleteAppContainerProfile.
"""
from __future__ import annotations

import ctypes
import msvcrt
import os
import subprocess
import threading
import time
import uuid
import winreg
from ctypes import wintypes
from dataclasses import dataclass, field
from pathlib import Path

PREFIX = "NC-Sandbox-spike-"
PY_BASE = r"C:\Program Files\Python312\python.exe"
HERE = Path(__file__).resolve().parent
SITE = str(HERE / ".venv" / "Lib" / "site-packages")

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
adv = ctypes.WinDLL("advapi32", use_last_error=True)
uenv = ctypes.WinDLL("userenv", use_last_error=True)
ole = ctypes.WinDLL("ole32")

HRESULT = ctypes.c_long
E_ALREADY_EXISTS = -2147024713  # HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)

ATTR_HANDLE_LIST = 0x20002
ATTR_SECURITY_CAPABILITIES = 0x20009
ATTR_JOB_LIST = 0x2000D
ATTR_CHILD_PROCESS_POLICY = 0x2000E
ATTR_ALL_APP_PACKAGES_POLICY = 0x2000F

EXTENDED_STARTUPINFO_PRESENT = 0x00080000
CREATE_UNICODE_ENVIRONMENT = 0x00000400
CREATE_NO_WINDOW = 0x00000008  # actually DETACHED_PROCESS: no console, so no conhost.exe joins the job
BELOW_NORMAL_PRIORITY_CLASS = 0x00004000
STARTF_USESTDHANDLES = 0x100

LIMIT_ACTIVE_PROCESS = 0x8
LIMIT_PROCESS_MEMORY = 0x100
LIMIT_DIE_ON_UNHANDLED_EXCEPTION = 0x400
LIMIT_KILL_ON_JOB_CLOSE = 0x2000
JOB_MSG = {1: "END_OF_JOB_TIME", 2: "END_OF_PROCESS_TIME", 3: "ACTIVE_PROCESS_LIMIT", 4: "ACTIVE_PROCESS_ZERO",
           6: "NEW_PROCESS", 7: "EXIT_PROCESS", 8: "ABNORMAL_EXIT_PROCESS", 9: "PROCESS_MEMORY_LIMIT",
           10: "JOB_MEMORY_LIMIT"}
EXIT_KILLED_MEMORY = 0xE0000009
EXIT_KILLED_TIMEOUT = 0xE0000102


class SID_AND_ATTRIBUTES(ctypes.Structure):
    _fields_ = [("Sid", ctypes.c_void_p), ("Attributes", wintypes.DWORD)]


class SECURITY_CAPABILITIES(ctypes.Structure):
    _fields_ = [("AppContainerSid", ctypes.c_void_p), ("Capabilities", ctypes.POINTER(SID_AND_ATTRIBUTES)),
                ("CapabilityCount", wintypes.DWORD), ("Reserved", wintypes.DWORD)]


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("lpReserved", wintypes.LPWSTR), ("lpDesktop", wintypes.LPWSTR),
                ("lpTitle", wintypes.LPWSTR)] + [(n, wintypes.DWORD) for n in (
                    "dwX", "dwY", "dwXSize", "dwYSize", "dwXCountChars", "dwYCountChars", "dwFillAttribute",
                    "dwFlags")] + [("wShowWindow", wintypes.WORD), ("cbReserved2", wintypes.WORD),
                                   ("lpReserved2", ctypes.c_void_p), ("hStdInput", wintypes.HANDLE),
                                   ("hStdOutput", wintypes.HANDLE), ("hStdError", wintypes.HANDLE)]


class STARTUPINFOEXW(ctypes.Structure):
    _fields_ = [("StartupInfo", STARTUPINFOW), ("lpAttributeList", ctypes.c_void_p)]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [("hProcess", wintypes.HANDLE), ("hThread", wintypes.HANDLE),
                ("dwProcessId", wintypes.DWORD), ("dwThreadId", wintypes.DWORD)]


class IO_COUNTERS(ctypes.Structure):
    _fields_ = [(n, ctypes.c_ulonglong) for n in ("R", "W", "O", "RT", "WT", "OT")]


class BASIC_LIMIT(ctypes.Structure):
    _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD)]


class EXTENDED_LIMIT(ctypes.Structure):
    _fields_ = [("Basic", BASIC_LIMIT), ("Io", IO_COUNTERS), ("ProcessMemoryLimit", ctypes.c_size_t),
                ("JobMemoryLimit", ctypes.c_size_t), ("PeakProcessMemoryUsed", ctypes.c_size_t),
                ("PeakJobMemoryUsed", ctypes.c_size_t)]


class ASSOC_PORT(ctypes.Structure):
    _fields_ = [("CompletionKey", ctypes.c_void_p), ("CompletionPort", wintypes.HANDLE)]


uenv.CreateAppContainerProfile.restype = HRESULT
uenv.CreateAppContainerProfile.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.LPCWSTR,
                                           ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(ctypes.c_void_p)]
uenv.DeleteAppContainerProfile.restype = HRESULT
uenv.DeleteAppContainerProfile.argtypes = [wintypes.LPCWSTR]
uenv.DeriveAppContainerSidFromAppContainerName.restype = HRESULT
uenv.DeriveAppContainerSidFromAppContainerName.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(ctypes.c_void_p)]
uenv.GetAppContainerFolderPath.restype = HRESULT
uenv.GetAppContainerFolderPath.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(wintypes.LPWSTR)]
adv.FreeSid.argtypes = [ctypes.c_void_p]
adv.ConvertSidToStringSidW.argtypes = [ctypes.c_void_p, ctypes.POINTER(wintypes.LPWSTR)]
k32.LocalFree.argtypes = [ctypes.c_void_p]
ole.CoTaskMemFree.argtypes = [ctypes.c_void_p]
k32.InitializeProcThreadAttributeList.argtypes = [ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD,
                                                  ctypes.POINTER(ctypes.c_size_t)]
k32.UpdateProcThreadAttribute.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.c_size_t, ctypes.c_void_p,
                                          ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p]
k32.DeleteProcThreadAttributeList.argtypes = [ctypes.c_void_p]
k32.CreateProcessW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, ctypes.c_void_p, ctypes.c_void_p, wintypes.BOOL,
                               wintypes.DWORD, ctypes.c_void_p, wintypes.LPCWSTR,
                               ctypes.POINTER(STARTUPINFOEXW), ctypes.POINTER(PROCESS_INFORMATION)]
k32.CreateJobObjectW.restype = wintypes.HANDLE
k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
k32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
k32.QueryInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD,
                                          ctypes.c_void_p]
k32.TerminateJobObject.argtypes = [wintypes.HANDLE, wintypes.UINT]
k32.CreateIoCompletionPort.restype = wintypes.HANDLE
k32.CreateIoCompletionPort.argtypes = [wintypes.HANDLE, wintypes.HANDLE, ctypes.c_size_t, wintypes.DWORD]
k32.GetQueuedCompletionStatus.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD),
                                          ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_void_p),
                                          wintypes.DWORD]
k32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
k32.WaitForSingleObject.restype = wintypes.DWORD
k32.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
k32.CloseHandle.argtypes = [wintypes.HANDLE]
k32.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]


def _check(hr: int, what: str) -> None:
    if hr != 0:
        raise OSError(f"{what} failed: HRESULT {hr & 0xFFFFFFFF:#010x}")


def sid_string(psid: int) -> str:
    s = wintypes.LPWSTR()
    if not adv.ConvertSidToStringSidW(psid, ctypes.byref(s)):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return s.value
    finally:
        k32.LocalFree(s)


def create_profile(name: str) -> int:
    psid = ctypes.c_void_p()
    hr = uenv.CreateAppContainerProfile(name, name, "Network Connector spike sandbox", None, 0, ctypes.byref(psid))
    if hr == E_ALREADY_EXISTS:
        _check(uenv.DeriveAppContainerSidFromAppContainerName(name, ctypes.byref(psid)), "DeriveSid")
    else:
        _check(hr, "CreateAppContainerProfile")
    return psid.value


def delete_profile(name: str) -> int:
    return uenv.DeleteAppContainerProfile(name)


def folder_path(sid_str: str) -> str:
    p = wintypes.LPWSTR()
    _check(uenv.GetAppContainerFolderPath(sid_str, ctypes.byref(p)), "GetAppContainerFolderPath")
    try:
        return p.value
    finally:
        ole.CoTaskMemFree(p)


def make_job(mem_mb: int, active_limit: int) -> int:
    job = k32.CreateJobObjectW(None, None)
    if not job:
        raise ctypes.WinError(ctypes.get_last_error())
    info = EXTENDED_LIMIT()
    flags = LIMIT_KILL_ON_JOB_CLOSE | LIMIT_DIE_ON_UNHANDLED_EXCEPTION
    if mem_mb:
        flags |= LIMIT_PROCESS_MEMORY
        info.ProcessMemoryLimit = mem_mb * 1024 * 1024
    if active_limit:
        flags |= LIMIT_ACTIVE_PROCESS
        info.Basic.ActiveProcessLimit = active_limit
    info.Basic.LimitFlags = flags
    if not k32.SetInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info)):
        raise ctypes.WinError(ctypes.get_last_error())
    ui = wintypes.DWORD(0xFF)  # JOB_OBJECT_UILIMIT_ALL: handles, clipboard, atoms, desktop, exit windows, ...
    k32.SetInformationJobObject(job, 4, ctypes.byref(ui), 4)
    return job


@dataclass
class Run:
    name: str = ""
    sid: str = ""
    folder: str = ""
    exit_code: int | None = None
    stdout: bytes = b""
    stderr: bytes = b""
    job_msgs: list[str] = field(default_factory=list)
    peak_mb: float = 0.0
    timed_out: bool = False
    t: dict[str, float] = field(default_factory=dict)
    leftover_after_delete: list[str] = field(default_factory=list)
    delete_hr: int = 0


def _attr_list(count: int) -> ctypes.Array:
    size = ctypes.c_size_t()
    k32.InitializeProcThreadAttributeList(None, count, 0, ctypes.byref(size))
    buf = (ctypes.c_byte * size.value)()
    if not k32.InitializeProcThreadAttributeList(buf, count, 0, ctypes.byref(size)):
        raise ctypes.WinError(ctypes.get_last_error())
    return buf


def run(cmd: list[str], stdin: bytes = b"", *, timeout: float = 30, mem_mb: int = 256, active_limit: int = 1,
        appcontainer: bool = True, lpac: bool = False, child_policy: bool = False, job: bool = True,
        kill_on_memory: bool = True, extra_env: dict[str, str] | None = None, cwd: str | None = None,
        wait_ready: bool = False, on_start=None) -> Run:
    r = Run()
    t0 = time.perf_counter()
    psid = None
    if appcontainer:
        r.name = PREFIX + uuid.uuid4().hex[:12]
        psid = create_profile(r.name)
        r.sid = sid_string(psid)
        r.folder = folder_path(r.sid)
    r.t["profile_ms"] = (time.perf_counter() - t0) * 1000
    hjob = make_job(mem_mb, active_limit) if job else None
    port, watcher, stop = None, None, threading.Event()
    if hjob:
        port = k32.CreateIoCompletionPort(wintypes.HANDLE(-1), None, 0, 1)
        assoc = ASSOC_PORT(None, port)
        k32.SetInformationJobObject(hjob, 7, ctypes.byref(assoc), ctypes.sizeof(assoc))

        def watch() -> None:
            msg, key, ov = wintypes.DWORD(), ctypes.c_size_t(), ctypes.c_void_p()
            while True:
                if k32.GetQueuedCompletionStatus(port, ctypes.byref(msg), ctypes.byref(key), ctypes.byref(ov), 50):
                    name = JOB_MSG.get(msg.value, str(msg.value))
                    r.job_msgs.append(name)
                    if kill_on_memory and msg.value in (9, 10):
                        k32.TerminateJobObject(hjob, EXIT_KILLED_MEMORY)
                elif stop.is_set():
                    return
        watcher = threading.Thread(target=watch, daemon=True)
        watcher.start()

    workdir = cwd or (r.folder or str(HERE))
    # CreateProcess fails with ERROR_ENVVAR_NOT_FOUND (203) for AppContainers unless LOCALAPPDATA is present;
    # Windows then rewrites LOCALAPPDATA/TEMP/TMP to the container folder.
    env = {"SystemRoot": os.environ["SystemRoot"], "LOCALAPPDATA": os.environ["LOCALAPPDATA"],
           "TEMP": workdir, "TMP": workdir, "PROCESSOR_ARCHITECTURE": "AMD64"}  # platform.machine(): WMI is blocked
    env.update(extra_env or {})
    env_block = ctypes.create_unicode_buffer("".join(f"{k}={v}\0" for k, v in env.items()))

    in_r, in_w = os.pipe()
    out_r, out_w = os.pipe()
    err_r, err_w = os.pipe()
    child = [msvcrt.get_osfhandle(fd) for fd in (in_r, out_w, err_w)]
    for h in child:
        os.set_handle_inheritable(h, True)

    keep = []
    n_attr = 1 + bool(appcontainer) + bool(hjob) + bool(child_policy) + bool(lpac)
    alist = _attr_list(n_attr)
    handles = (wintypes.HANDLE * 3)(*child)
    keep.append(handles)
    k32.UpdateProcThreadAttribute(alist, 0, ATTR_HANDLE_LIST, handles, ctypes.sizeof(handles), None, None)
    if appcontainer:
        caps = SECURITY_CAPABILITIES(psid, None, 0, 0)
        keep.append(caps)
        if not k32.UpdateProcThreadAttribute(alist, 0, ATTR_SECURITY_CAPABILITIES, ctypes.byref(caps),
                                             ctypes.sizeof(caps), None, None):
            raise ctypes.WinError(ctypes.get_last_error())
    if hjob:
        jl = (wintypes.HANDLE * 1)(hjob)
        keep.append(jl)
        if not k32.UpdateProcThreadAttribute(alist, 0, ATTR_JOB_LIST, jl, ctypes.sizeof(jl), None, None):
            raise ctypes.WinError(ctypes.get_last_error())
    if child_policy:
        cp = wintypes.DWORD(1)  # PROCESS_CREATION_CHILD_PROCESS_RESTRICTED
        keep.append(cp)
        k32.UpdateProcThreadAttribute(alist, 0, ATTR_CHILD_PROCESS_POLICY, ctypes.byref(cp), 4, None, None)
    if lpac:
        ap = wintypes.DWORD(1)  # PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
        keep.append(ap)
        k32.UpdateProcThreadAttribute(alist, 0, ATTR_ALL_APP_PACKAGES_POLICY, ctypes.byref(ap), 4, None, None)

    si = STARTUPINFOEXW()
    si.StartupInfo.cb = ctypes.sizeof(si)
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES
    si.StartupInfo.hStdInput, si.StartupInfo.hStdOutput, si.StartupInfo.hStdError = child
    si.lpAttributeList = ctypes.cast(alist, ctypes.c_void_p)
    pi = PROCESS_INFORMATION()
    cmdline = ctypes.create_unicode_buffer(subprocess.list2cmdline(cmd))
    flags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
    t1 = time.perf_counter()
    ok = k32.CreateProcessW(cmd[0], cmdline, None, None, True, flags, env_block, workdir, ctypes.byref(si),
                            ctypes.byref(pi))
    err = ctypes.get_last_error()
    t2 = time.perf_counter()
    r.t["launch_ms"] = (t2 - t1) * 1000
    k32.DeleteProcThreadAttributeList(alist)
    for fd in (in_r, out_w, err_w):
        os.close(fd)
    if not ok:
        for fd in (in_w, out_r, err_r):
            os.close(fd)
        _cleanup(r, hjob, port, stop, watcher)
        raise ctypes.WinError(err)
    if on_start:
        on_start(r, pi.dwProcessId)

    chunks: dict[str, list[bytes]] = {"out": [], "err": []}
    ready = threading.Event()
    t_feed = [0.0]

    def pump(fd: int, key: str) -> None:
        while data := os.read(fd, 65536):
            chunks[key].append(data)
            if key == "err" and not ready.is_set() and b"READY" in b"".join(chunks["err"]):
                r.t["ready_ms"] = (time.perf_counter() - t2) * 1000
                ready.set()
        os.close(fd)

    def feed() -> None:
        try:
            if wait_ready:
                ready.wait(timeout)
            t_feed[0] = time.perf_counter()
            if stdin:
                os.write(in_w, stdin)
        except OSError:
            pass
        finally:
            os.close(in_w)
    threads = [threading.Thread(target=pump, args=(out_r, "out")), threading.Thread(target=pump, args=(err_r, "err")),
               threading.Thread(target=feed)]
    for t in threads:
        t.start()
    if k32.WaitForSingleObject(pi.hProcess, int(timeout * 1000)) == 0x102:
        r.timed_out = True
        if hjob:
            k32.TerminateJobObject(hjob, EXIT_KILLED_TIMEOUT)
        else:
            k32.TerminateProcess(pi.hProcess, EXIT_KILLED_TIMEOUT)
        k32.WaitForSingleObject(pi.hProcess, 5000)
    t3 = time.perf_counter()
    r.t["process_ms"] = (t3 - t2) * 1000
    if wait_ready:
        r.t["job_ms"] = (t3 - t_feed[0]) * 1000
    code = wintypes.DWORD()
    k32.GetExitCodeProcess(pi.hProcess, ctypes.byref(code))
    r.exit_code = code.value
    for t in threads:
        t.join(5)
    r.stdout, r.stderr = b"".join(chunks["out"]), b"".join(chunks["err"])
    if hjob:
        info = EXTENDED_LIMIT()
        if k32.QueryInformationJobObject(hjob, 9, ctypes.byref(info), ctypes.sizeof(info), None):
            r.peak_mb = info.PeakProcessMemoryUsed / 1024 / 1024
    k32.CloseHandle(pi.hThread)
    k32.CloseHandle(pi.hProcess)
    _cleanup(r, hjob, port, stop, watcher)
    if psid:
        adv.FreeSid(psid)
    r.t["total_ms"] = (time.perf_counter() - t0) * 1000
    return r


def _cleanup(r: Run, hjob, port, stop, watcher) -> None:
    t = time.perf_counter()
    if hjob:
        k32.CloseHandle(hjob)  # kill-on-close: anything left in the job dies here
    stop.set()
    if watcher:
        watcher.join(2)
    if port:
        k32.CloseHandle(port)
    if r.name:
        r.delete_hr = delete_profile(r.name)
        r.leftover_after_delete = [x for x in leftovers() if r.name.lower() in x.lower()]
    r.t["delete_ms"] = (time.perf_counter() - t) * 1000


MAPPINGS = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings"


def leftovers(prefix: str = PREFIX) -> list[str]:
    """Profiles whose folder or registry mapping still exists."""
    found = []
    pkgs = Path(os.environ["LOCALAPPDATA"]) / "Packages"
    for p in pkgs.iterdir():
        if p.name.lower().startswith(prefix.lower()):
            found.append(f"folder {p}")
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, MAPPINGS) as k:
            i = 0
            while True:
                try:
                    sub = winreg.EnumKey(k, i)
                except OSError:
                    break
                i += 1
                try:
                    with winreg.OpenKey(k, sub) as s:
                        moniker = winreg.QueryValueEx(s, "Moniker")[0]
                except OSError:
                    continue
                if moniker.lower().startswith(prefix.lower()):
                    found.append(f"registry {sub} ({moniker})")
    except FileNotFoundError:
        pass
    return found


def sweep(prefix: str = PREFIX) -> list[str]:
    """Start-up sweep: delete every profile (by registry moniker) and orphan folder with our prefix."""
    import shutil
    done = []
    for item in leftovers(prefix):
        if item.startswith("registry "):
            moniker = item.rsplit("(", 1)[1].rstrip(")")
            done.append(f"DeleteAppContainerProfile({moniker}) -> {delete_profile(moniker) & 0xFFFFFFFF:#x}")
    for item in leftovers(prefix):
        if item.startswith("folder "):
            shutil.rmtree(item[7:], ignore_errors=True)
            done.append(f"rmtree {item[7:]}")
    return done


def runner_cmd(module: str, *args: str) -> list[str]:
    return [PY_BASE, "-I", "-S", "-B", str(HERE / "runner.py"), "--site", SITE, "--module", str(HERE / "build" / module),
            *args]


def probe_cmd(*args: str) -> list[str]:
    return [PY_BASE, "-I", "-S", "-B", str(HERE / "probe.py"), *args]
