"""The worker agent: connects OUT to the controller, pulls jobs, runs them with resource limits.

The worker opens no listening port, so nothing on the network can connect to it.
"""
from __future__ import annotations

import base64
import json
import logging
import os
import subprocess
import sys
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import httpx
import psutil

from . import __version__, jobs, modules, sandbox
from .client import ControllerError, controller_url, fetch_module, pinned_context, probe, raise_for_error
from .config import WorkerConfig, load_config, save_config
from .helper_policy import limit_reason
from .helper_usage import OUTCOME_KIND, HelperUsage
from .security import constant_time_equal, fingerprint_pem_file
from .sysinfo import hardware_info, live_metrics, prime_cpu_counter, speed_score
from .winlimits import limit_memory, release, peak_memory_mb

log = logging.getLogger("network_connector.worker")

BACKOFF = (1, 2, 4, 8, 15, 30)
CREATE_NO_WINDOW = 0x08000000
BELOW_NORMAL_PRIORITY_CLASS = 0x00004000
_THREAD_VARS = ("OMP_NUM_THREADS", "MKL_NUM_THREADS", "OPENBLAS_NUM_THREADS", "NUMEXPR_NUM_THREADS")


class NotPaired(RuntimeError):
    pass


def load_worker_config(wdir: Path) -> WorkerConfig:
    cfg = load_config(WorkerConfig, wdir / "worker.json")
    if not (cfg.token and cfg.controller_host and (wdir / "controller.crt").is_file()):
        raise NotPaired("This worker is not paired yet. Pair it with a code from the controller first.")
    # Upgrade older pairings from the already trusted certificate, without fetching a new one.
    fingerprint = fingerprint_pem_file(wdir / "controller.crt")
    known = list(dict.fromkeys([cfg.controller_host, *cfg.known_hosts]))[:4]
    if cfg.controller_fingerprint != fingerprint or cfg.known_hosts != known:
        cfg.controller_fingerprint, cfg.known_hosts = fingerprint, known
        save_config(cfg, wdir / "worker.json")
    return cfg


def _python_for_children() -> str:
    exe = Path(sys.executable)
    if exe.name.lower() == "pythonw.exe" and (exe.parent / "python.exe").is_file():
        return str(exe.parent / "python.exe")
    return str(exe)


@dataclass
class RunningJob:
    id: str
    kind: str
    attempt: int
    timeout_s: int
    proc: subprocess.Popen | None
    handle: object | None
    started: float = field(default_factory=time.monotonic)
    progress: float = 0.0
    stdout: bytes = b""
    cancel: bool = False
    abandon: bool = False
    cpu_seconds: float | None = None
    peak_mem_mb: float = 0.0
    threads: list[threading.Thread] = field(default_factory=list)
    sandbox: Any = None  # wasm jobs: a sandbox.Sandbox run on threads[0]
    wasm_result: Any = None
    wasm_error: str = ""
    module_sha256: str = ""
    label: str = ""  # the requester's batch label, shown in "what is running"


class WorkerAgent:
    def __init__(self, cfg: WorkerConfig, wdir: Path) -> None:
        self.cfg, self.dir = cfg, wdir
        self.work_dir = wdir / "work"
        self.work_dir.mkdir(parents=True, exist_ok=True)
        self.stop_event = threading.Event()
        self.running: dict[str, RunningJob] = {}
        self.pending: list[tuple[str, str, dict[str, Any]]] = []  # reports not yet delivered
        self.local_paused = False
        self.remote_paused = False
        self.remote_disabled = False
        self._revoked = False
        self.remote_closing = False  # the requester stopped handing out work (PLAN §13)
        self._leave = False
        self._immediate_stop = False
        self.state = "starting"
        self.last_error = ""
        self.completed = 0
        self.failed = 0
        self.events: deque[str] = deque(maxlen=300)
        self.event_count = 0
        self._thread: threading.Thread | None = None
        self._http: httpx.Client | None = None
        self._rtt_ms: float | None = None
        self._link = "unknown"
        self.module_cache = modules.ModuleCache(wdir / "modules")
        self.usage = HelperUsage(wdir / "helper_usage.db")
        # The requester's heartbeat can only make cfg.group_mode stricter, never looser.
        self._remote_group_mode = "closed"
        self._sandbox_note = ""
        self._policy_note = ""
        self._next_limit_check = 0.0
        self._info = hardware_info() | {
            "limits": {
                "max_cpu_threads": cfg.max_cpu_threads,
                "max_concurrent_jobs": cfg.max_concurrent_jobs,
                "max_memory_mb": cfg.max_memory_mb,
                "low_priority": cfg.low_priority,
            }
        }

    # ------------------------------------------------------------ public controls
    def start_thread(self) -> None:
        self._thread = threading.Thread(target=self.run, name="worker-agent", daemon=True)
        self._thread.start()

    def stop(self, wait: bool = True) -> None:
        self.stop_event.set()
        if wait and self._thread and self._thread is not threading.current_thread():
            self._thread.join(timeout=self.cfg.shutdown_grace_s + 15)

    def leave(self, wait: bool = True) -> None:
        """Stop, hand unfinished jobs back, and tell the requester this helper leaves (its token is dropped)."""
        self._leave = True
        self.stop(wait)

    def stop_helping(self) -> None:
        """Owner emergency Stop: refuse new claims and kill active sandboxes immediately."""
        self.pause()
        self._immediate_stop = True
        self._leave = self.cfg.group_mode == "open"
        sandbox.stop_all()
        self.stop_event.set()

    def pause(self) -> None:
        self.local_paused = True
        self._event("paused (no new jobs will be started)")

    def resume(self) -> None:
        self.local_paused = False
        self._event("resumed")

    @property
    def slots(self) -> int:
        return max(1, min(self.cfg.max_concurrent_jobs, self.cfg.max_cpu_threads))

    def snapshot(self) -> dict[str, Any]:
        running = []
        for j in list(self.running.values()):
            pid = j.sandbox.pid if j.sandbox is not None else j.proc.pid if j.proc is not None else 0
            cpu_percent = ram_mb = 0.0
            if pid:
                try:
                    process = psutil.Process(pid)
                    cpu_percent = process.cpu_percent(interval=None)
                    ram_mb = process.memory_info().rss / (1024 * 1024)
                except psutil.Error:
                    pass
            running.append({"id": j.id, "kind": j.kind, "label": j.label, "progress": round(j.progress, 1),
                            "elapsed": round(time.monotonic() - j.started, 1),
                            "cpu_percent": round(cpu_percent, 1), "ram_mb": round(ram_mb, 1)}
                           | ({"module_sha256": j.module_sha256} if j.module_sha256 else {}))
        return {
            "state": self.state, "paused": self.local_paused or self.remote_paused, "disabled": self.remote_disabled,
            "running": running,
            "completed": self.completed, "failed": self.failed, "last_error": self.last_error,
            "slots": self.slots, "metrics": live_metrics(), "name": self.cfg.name,
            "link": self._link, "controller_host": self.cfg.controller_host, "rtt_ms": self._rtt_ms,
            "group_mode": self.cfg.group_mode, "requester_name": self.cfg.requester_name, "closing": self.remote_closing,
            "requester_id": (self.cfg.requester_device_id or self.cfg.controller_fingerprint)[:8],
            "cpu_minutes_today": round(self._used_today() / 60, 1),
            "daily_cpu_min": self.cfg.daily_cpu_min,
            "requester_reputation": self.usage.reputation(self._requester_key()),
            "policy_reason": self._policy_note,
        }

    # ------------------------------------------------------------ main loop
    @property
    def effective_group_mode(self) -> str:
        """'open' (wasm only) if this connection was made through /join (cfg.group_mode, set by the Link
        agent's join) or the requester says so; read live so a later join applies at once."""
        own = getattr(self.cfg, "group_mode", "closed")
        return "open" if "open" in (own, self._remote_group_mode) else "closed"

    def _share_speed(self) -> None:
        try:
            from .paths import data_home

            self._info["speed_units_per_s"] = speed_score(data_home() / "speed.json", __version__)
        except Exception:  # noqa: BLE001 - the score is optional
            pass

    def run(self) -> None:
        prime_cpu_counter()
        if self.cfg.share_speed_score:  # only with the owner's permission (PLAN §19)
            threading.Thread(target=self._share_speed, name="speed-score", daemon=True).start()
        try:
            swept = sandbox.sweep()  # leftovers from a crash or power loss
            if swept:
                self._event(f"removed {swept} leftover sandbox(es) from an earlier run")
        except OSError:
            pass
        self._http = self._new_client(self.cfg.controller_host)
        self.state = "connecting"
        self._event(f"worker '{self.cfg.name}' starting; controller {self.cfg.controller_host}:{self.cfg.controller_port}")
        next_hb = next_claim = 0.0
        backoff_i = 0
        last_activity = time.monotonic()
        try:
            while not self.stop_event.is_set():
                self._read_control_file()
                self._check_limits_while_running()
                finished = self._poll_running()
                if finished:
                    last_activity = time.monotonic()
                    next_claim = 0.0
                now = time.monotonic()
                try:
                    if now >= next_hb:
                        self._heartbeat()
                        self._flush_pending()
                        next_hb = now + self.cfg.heartbeat_s
                        backoff_i = 0
                        if self.state != "stopping":
                            self.state = "paused" if (self.local_paused or self.remote_paused) else "online"
                            if self.remote_disabled:
                                self.state = "disabled"
                    if now >= next_claim and self._can_claim():
                        if self._claim():
                            last_activity = time.monotonic()
                        next_claim = now + self.cfg.poll_s
                except ControllerError as exc:
                    if exc.status == 401:
                        self._revoked = True
                        self.last_error = "This worker was revoked or its token is no longer valid."
                        self._event(self.last_error + " Stopping.")
                        break
                    self._net_problem(exc)
                    next_hb = time.monotonic() + BACKOFF[min(backoff_i, len(BACKOFF) - 1)]
                    backoff_i += 1
                except (httpx.HTTPError, OSError) as exc:
                    self._net_problem(exc)
                    next_hb = time.monotonic() + BACKOFF[min(backoff_i, len(BACKOFF) - 1)]
                    backoff_i += 1
                    self.state = "offline"
                    if backoff_i >= 3 and self._try_refind():
                        next_hb = next_claim = 0.0
                        backoff_i = 0
                if (self.cfg.idle_timeout_minutes > 0 and not self.running
                        and time.monotonic() - last_activity > self.cfg.idle_timeout_minutes * 60):
                    self._event(f"idle for {self.cfg.idle_timeout_minutes} minutes; stopping (idle timeout)")
                    break
                if self.remote_closing and self.cfg.group_mode == "open" and not self.running:
                    # Joined groups end when the requester closes; paired (closed) groups wait for it to come back.
                    self._event("the requester is closing; leaving its group")
                    self._leave = True
                    break
                self.stop_event.wait(0.25)
        finally:
            self._graceful_shutdown()
            if self._leave and not self._revoked:
                self._send_leave()
            if self._http:
                self._http.close()
            self.state = "revoked" if self._revoked else "left" if self._leave else "stopped"
            self._write_status()
            self._event("worker stopped")

    # ------------------------------------------------------------ network helpers
    def _new_client(self, host: str) -> httpx.Client:
        return httpx.Client(
            base_url=controller_url(host, self.cfg.controller_port),
            verify=pinned_context(self.dir / "controller.crt"),
            headers={"Authorization": f"Bearer {self.cfg.token}"}, timeout=10,
        )

    def _try_refind(self) -> bool:
        from .wifidirect import candidate_controllers

        try:
            expected = fingerprint_pem_file(self.dir / "controller.crt")
            candidates = list(dict.fromkeys([*self.cfg.known_hosts[:4], *candidate_controllers()]))[:8]
            for host in candidates:
                if self.stop_event.is_set():
                    return False
                if host == self.cfg.controller_host:
                    continue
                seen = probe(host, self.cfg.controller_port, timeout=2)
                if seen is None or not constant_time_equal(seen, expected):
                    continue
                http = self._new_client(host)
                previous = self.cfg.controller_host, self.cfg.controller_fingerprint, self.cfg.known_hosts
                self.cfg.controller_host, self.cfg.controller_fingerprint = host, expected
                self.cfg.known_hosts = list(dict.fromkeys([host, previous[0], *previous[2]]))[:4]
                try:
                    save_config(self.cfg, self.dir / "worker.json")
                except OSError:
                    self.cfg.controller_host, self.cfg.controller_fingerprint, self.cfg.known_hosts = previous
                    http.close()
                    return False
                old, self._http = self._http, http
                if old:
                    old.close()
                self._event(f"found the pinned controller at {host}:{self.cfg.controller_port}")
                return True
        except (OSError, ValueError, RuntimeError):
            pass
        return False

    def _call(self, path: str, body: dict[str, Any]) -> dict[str, Any]:
        assert self._http is not None
        started = time.monotonic()
        response = raise_for_error(self._http.post(path, json=body))
        self._rtt_ms = (time.monotonic() - started) * 1000
        return response

    def _net_problem(self, exc: Exception) -> None:
        msg = str(exc) or type(exc).__name__
        if msg != self.last_error:
            self._event(f"cannot reach controller: {msg}. Will keep retrying; running jobs continue.")
        self.last_error = msg
        if self.state != "stopping":
            self.state = "offline"

    def _heartbeat(self) -> None:
        self._info["sandbox"] = bool(sandbox.available()[0])  # cached; lets the requester plan wasm benchmarks
        resp = self._call("/worker/heartbeat", {
            "info": self._info,
            "metrics": live_metrics() | {"running_jobs": len(self.running), "rtt_ms": self._rtt_ms},
            "version": __version__,
            "jobs": [{"id": j.id, "attempt": j.attempt, "progress": j.progress} for j in self.running.values()],
        })
        if self.last_error and self.state in ("offline", "connecting"):
            self._event("connected to controller")
        self.last_error = ""
        self._link = resp.get("link", "unknown")
        for jid in resp.get("cancel", []):
            if jid in self.running and not self.running[jid].cancel:
                self.running[jid].cancel = True
                self._event(f"cancel requested for job {jid}")
        for jid in resp.get("abandon", []):
            if jid in self.running:
                self.running[jid].abandon = True
        self.remote_paused = bool(resp.get("paused"))
        self.remote_disabled = bool(resp.get("disabled"))
        closing = bool(resp.get("closing"))
        if closing and not self.remote_closing:
            self._event("the requester is closing: no new jobs; running jobs may finish")
        self.remote_closing = closing
        self._remote_group_mode = "open" if resp.get("group_mode") == "open" else "closed"
        self._write_status()

    def _can_claim(self) -> bool:
        if self.stop_event.is_set() or self.local_paused or self.remote_paused or self.remote_disabled or self.remote_closing:
            return False
        if len(self.running) >= self.slots or self.state in ("stopping", "offline", "connecting"):
            return False
        if self._limit_note():
            return False
        return psutil.virtual_memory().percent < 92  # never start work when the laptop is nearly out of RAM

    def _requester_key(self) -> str:
        return self.cfg.requester_device_id or self.cfg.controller_fingerprint

    def _requester_blocked(self) -> bool:
        from .identity import PeerBook

        key = self._requester_key()
        if not key:
            return False
        if key in self.cfg.blocked_requesters:
            return True
        try:
            return PeerBook().is_blocked(key)
        except (OSError, ValueError):
            return False

    @staticmethod
    def _job_cpu(j: RunningJob) -> float:
        pid = j.sandbox.pid if j.sandbox is not None else j.proc.pid if j.proc is not None else 0
        if not pid:
            return 0.0
        try:
            times = psutil.Process(pid).cpu_times()
            return times.user + times.system
        except psutil.Error:
            return 0.0

    def _running_cpu_seconds(self) -> float:
        """CPU time the running jobs used so far; counted against the daily limit before they finish."""
        return sum(self._job_cpu(j) for j in list(self.running.values()))

    def _used_today(self) -> float:
        return self.usage.seconds(self._requester_key()) + self._running_cpu_seconds()

    def _limit_note(self) -> str:
        """Checks the owner's limits on this helper; records and announces a new reason."""
        reason = limit_reason(self.cfg, self._used_today(), self._requester_blocked())
        if reason and reason != self._policy_note:
            self._event(reason + ("" if not self.running else
                                  "; stopping the running job(s) now" if self.cfg.limit_stop_now or
                                  reason.startswith("requester blocked") else "; the running job(s) may finish"))
        self._policy_note = reason
        return reason

    def _check_limits_while_running(self) -> None:
        """PLAN §16.2: limits also apply while jobs run. Blocking always stops them; other limits only
        when the owner chose "stop immediately"."""
        now = time.monotonic()
        if not self.running or now < self._next_limit_check:
            return
        self._next_limit_check = now + 1.0
        reason = self._limit_note()
        if not reason or not (self.cfg.limit_stop_now or reason.startswith("requester blocked")):
            return
        for rj in list(self.running.values()):  # only this requester's jobs; _kill stops each one's own sandbox
            used = self._job_cpu(rj)
            self._kill(rj)
            self._drop(rj)
            self.usage.add(self._requester_key(), used)
            try:
                self._call(f"/worker/jobs/{rj.id}/release", {"attempt": rj.attempt})
                self._event(f"job {rj.id} stopped ({reason}); handed back to the requester")
            except Exception:  # noqa: BLE001 - its lease expires and it is re-queued anyway
                pass

    def claim_kinds(self) -> list[str]:
        """Job kinds this helper accepts right now: by group mode, and wasm only if the sandbox works."""
        kinds = set(jobs.allowed_kinds(self.effective_group_mode))
        if jobs.WASM in kinds:
            ok, reason = sandbox.available()
            if not ok:
                kinds.discard(jobs.WASM)
                if reason != self._sandbox_note:
                    self._event(f"not taking wasm jobs: {reason}")
            self._sandbox_note = "" if ok else reason
        return sorted(kinds)

    def _claim(self) -> bool:
        kinds = self.claim_kinds()
        if not kinds:
            return False
        resp = self._call("/worker/claim", {"max_jobs": self.slots - len(self.running), "kinds": kinds})
        for job in resp.get("jobs", []):
            self._start_job(job)
        return bool(resp.get("jobs"))

    def _report(self, job_id: str, action: str, body: dict[str, Any]) -> None:
        try:
            self._call(f"/worker/jobs/{job_id}/{action}", body)
        except ControllerError as exc:
            if exc.status == 401:
                raise
            self._event(f"controller ignored report for job {job_id}: {exc.message}")  # stale (409) or unknown (404)
        except (httpx.HTTPError, OSError):
            self.pending.append((job_id, action, body))
            self._event(f"controller unreachable; will deliver result of job {job_id} when reconnected")

    def _flush_pending(self) -> None:
        pending, self.pending = self.pending, []
        for job_id, action, body in pending:
            self._report(job_id, action, body)

    # ------------------------------------------------------------ running jobs
    def _refuse(self, job: dict[str, Any], error: str, retryable: bool) -> None:
        self.failed += 1
        self._event(f"refused job {job['id']}: {error}")
        try:
            self._report(job["id"], "fail", {"attempt": job["attempt"], "error": error[:1900], "retryable": retryable})
        except ControllerError:
            pass

    def _start_job(self, job: dict[str, Any]) -> None:
        if job.get("kind") not in jobs.allowed_kinds(self.effective_group_mode):
            self._refuse(job, f"open groups run only sandboxed wasm jobs; {job.get('kind')!r} is refused on this helper", False)
            return
        if job["kind"] == jobs.WASM:
            self._start_wasm(job)
            return
        env = os.environ.copy()
        for var in _THREAD_VARS:
            env[var] = "1"
        package_parent = str(Path(__file__).resolve().parent.parent)
        env["PYTHONPATH"] = package_parent + os.pathsep + env.get("PYTHONPATH", "")
        flags = 0
        if sys.platform == "win32":
            flags = CREATE_NO_WINDOW | (BELOW_NORMAL_PRIORITY_CLASS if self.cfg.low_priority else 0)
        proc = subprocess.Popen(
            [_python_for_children(), "-m", "network_connector.jobrunner"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            cwd=self.work_dir, env=env, creationflags=flags,
        )
        if sys.platform != "win32" and self.cfg.low_priority:
            try:
                psutil.Process(proc.pid).nice(10)
            except psutil.Error:
                pass
        rate = self._cpu_rate_per_job()
        handle = limit_memory(proc.pid, self.cfg.max_memory_mb, rate)
        if rate and handle is None:
            proc.kill()
            proc.wait(timeout=10)
            self._refuse(job, "the owner's CPU-rate limit could not be enforced", True)
            return
        rj = RunningJob(job["id"], job["kind"], job["attempt"], job["timeout_s"], proc, handle,
                        label=str(job.get("label") or "")[:120])
        payload = json.dumps({"kind": job["kind"], "params": job["params"]}).encode()

        def feed() -> None:
            try:
                assert proc.stdin is not None
                proc.stdin.write(payload)
                proc.stdin.close()
            except OSError:
                pass

        def read_out() -> None:
            assert proc.stdout is not None
            rj.stdout = proc.stdout.read()

        def read_err() -> None:
            assert proc.stderr is not None
            for line in proc.stderr:
                if line.startswith(b"PROGRESS "):
                    try:
                        rj.progress = float(line[9:].strip())
                    except ValueError:
                        pass
                elif line.startswith(b"METRICS "):
                    try:
                        rj.cpu_seconds = max(0.0, float(json.loads(line[8:])["cpu_s"]))
                    except (ValueError, KeyError, TypeError):
                        pass

        rj.threads = [threading.Thread(target=f, daemon=True) for f in (feed, read_out, read_err)]
        for t in rj.threads:
            t.start()
        self.running[rj.id] = rj
        self._event(f"started job {rj.id} ({rj.kind}, attempt {rj.attempt})")

    def _cpu_rate_per_job(self) -> int:
        """The owner's whole-helper CPU cap, shared out between the job slots (at least 1 %)."""
        if not self.cfg.cpu_rate_percent:
            return 0
        return max(1, min(100, self.cfg.cpu_rate_percent // self.slots))

    def _module(self, sha256: str) -> bytes:
        data = self.module_cache.get(sha256)
        if data is None:
            assert self._http is not None
            data = fetch_module(self._http, sha256, modules.MAX_MODULE_BYTES)
            self.module_cache.put(sha256, data)  # checks size, hash and imports first
        return data

    def _start_wasm(self, job: dict[str, Any]) -> None:
        ok, reason = sandbox.available()
        if not ok:
            self._refuse(job, f"this helper can't run wasm jobs: {reason}", True)
            return
        try:
            params = jobs.validate(jobs.WASM, job["params"])
        except jobs.JobError as exc:
            self._refuse(job, f"job rejected: {exc}", False)
            return
        limits = params["limits"]
        if limits["memory_mb"] > self.cfg.max_memory_mb:
            self._refuse(job, f"needs {limits['memory_mb']} MB; this helper allows {self.cfg.max_memory_mb} MB per job", True)
            return
        try:
            module = self._module(params["module_sha256"])
        except modules.ModuleError as exc:
            self._refuse(job, f"module refused: {exc}", False)
            return
        except (ControllerError, httpx.HTTPError, OSError) as exc:
            self._refuse(job, f"could not download the module: {exc}", True)
            return
        box = sandbox.Sandbox(module, base64.b64decode(params["input_b64"]), params["args"],
                              memory_mb=limits["memory_mb"], seconds=min(limits["seconds"], job["timeout_s"]),
                              fuel=limits.get("fuel"), cpu_rate_percent=self._cpu_rate_per_job())
        rj = RunningJob(job["id"], job["kind"], job["attempt"], job["timeout_s"], None, None, sandbox=box,
                        module_sha256=params["module_sha256"], label=str(job.get("label") or "")[:120])

        def go() -> None:
            try:
                rj.wasm_result = box.run()
            except sandbox.SandboxUnavailable as exc:
                rj.wasm_error = f"this helper can't run wasm jobs: {exc}"
            except Exception as exc:  # noqa: BLE001 - reported as a retryable failure
                rj.wasm_error = f"sandbox error: {type(exc).__name__}: {exc}"

        rj.threads = [threading.Thread(target=go, name=f"sandbox-{rj.id}", daemon=True)]
        rj.threads[0].start()
        self.running[rj.id] = rj
        self._event(f"started job {rj.id} (wasm {rj.module_sha256[:12]}, attempt {rj.attempt}) in a fresh sandbox")

    def _poll_wasm(self, rj: RunningJob) -> bool:
        """Returns True when the job left self.running."""
        elapsed = time.monotonic() - rj.started
        if rj.threads[0].is_alive():
            if rj.abandon or rj.cancel:
                rj.sandbox.kill("cancelled")
            if not rj.abandon:
                rj.progress = min(99.0, elapsed * 100 / max(rj.sandbox.seconds, 1))
                return False
            rj.threads[0].join(5)
        self._drop(rj)
        if rj.abandon:
            return True
        r = rj.wasm_result
        action, body = "fail", {"attempt": rj.attempt}
        if r is None:
            body |= {"error": rj.wasm_error or "the sandbox failed", "retryable": True}
        elif r.outcome == "stopped" and not rj.cancel:
            self._event(f"job {rj.id} stopped by the owner; handing it back")
            self._report(rj.id, "release", {"attempt": rj.attempt})
            return True
        elif r.ok:
            action = "complete"
            body["result"] = {"stdout_b64": base64.b64encode(r.stdout).decode(), "exit_code": r.exit_code,
                              "fuel_used": r.fuel_used, "cpu_seconds": r.cpu_seconds,
                              "stderr_b64": base64.b64encode(r.stderr).decode()}
        elif rj.cancel or r.outcome == "cancelled":
            body |= {"error": "cancelled", "cancelled": True, "retryable": False}
        else:
            body |= {"error": f"wasm {r.outcome}: {r.detail}"[:1900], "retryable": r.outcome in ("timeout", "crashed", "error")}
        if r is not None:
            body |= {"cpu_seconds": min(r.cpu_seconds, 86400), "peak_mem_mb": r.peak_mem_mb or None}
            self.usage.add(self._requester_key(), max(0, r.cpu_seconds))
            kind = "escape" if r.escape_signal else OUTCOME_KIND.get(r.outcome)
            if kind:
                self.usage.note(self._requester_key(), kind)
            if r.escape_signal:
                self._event(f"job {rj.id} tried to start a process from inside the sandbox; it was blocked and killed")
        body["duration"] = round(elapsed, 3)
        if action == "complete":
            self.completed += 1
            self._event(f"job {rj.id} finished in {elapsed:.1f}s (sandbox deleted)")
        else:
            self.failed += 1
            self._event(f"job {rj.id} did not succeed: {body['error']} (sandbox deleted)")
        try:
            self._report(rj.id, action, body)
        except ControllerError:
            pass
        return True

    def _kill(self, rj: RunningJob) -> None:
        if rj.sandbox is not None:
            rj.sandbox.kill("stopped")
            for t in rj.threads:
                t.join(timeout=10)
            return
        try:
            assert rj.proc is not None
            rj.proc.kill()
            rj.proc.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            pass

    def _poll_running(self) -> bool:
        any_finished = False
        for rj in list(self.running.values()):
            if rj.sandbox is not None:
                any_finished = self._poll_wasm(rj) or any_finished
                continue
            elapsed = time.monotonic() - rj.started
            exit_code = rj.proc.poll()
            body: dict[str, Any] | None = None
            action = "fail"
            if exit_code is None:
                over_memory = False
                try:
                    memory = psutil.Process(rj.proc.pid).memory_info()
                    rj.peak_mem_mb = max(rj.peak_mem_mb, getattr(memory, "peak_wset", memory.rss) / 1024 / 1024)
                    over_memory = memory.rss > self.cfg.max_memory_mb * 1024 * 1024 * 1.15
                except psutil.Error:
                    pass
                if rj.abandon:
                    self._kill(rj)
                    self._drop(rj)
                    any_finished = True
                    continue
                if rj.cancel:
                    self._kill(rj)
                    body = {"attempt": rj.attempt, "error": "cancelled", "cancelled": True, "retryable": False}
                elif elapsed > rj.timeout_s:
                    self._kill(rj)
                    body = {"attempt": rj.attempt, "error": f"timed out after {rj.timeout_s}s", "retryable": True}
                elif over_memory:
                    self._kill(rj)
                    body = {"attempt": rj.attempt, "error": f"exceeded memory limit of {self.cfg.max_memory_mb} MB",
                            "retryable": False}
                else:
                    continue
            else:
                for t in rj.threads:
                    t.join(timeout=3)
                try:
                    out = json.loads(rj.stdout or b"{}")
                except ValueError:
                    out = {}
                if out.get("ok"):
                    action, body = "complete", {"attempt": rj.attempt, "result": out["result"], "duration": round(elapsed, 3)}
                elif out:
                    body = {"attempt": rj.attempt, "error": out.get("error", "job failed"),
                            "retryable": bool(out.get("retryable")), "duration": round(elapsed, 3)}
                else:
                    body = {"attempt": rj.attempt, "retryable": True, "duration": round(elapsed, 3),
                            "error": f"job process exited unexpectedly (code {exit_code}); it may have hit the memory limit"}
            body.update(duration=round(elapsed, 3), cpu_seconds=rj.cpu_seconds, peak_mem_mb=peak_memory_mb(rj.handle) or rj.peak_mem_mb or None)
            self.usage.add(self._requester_key(), max(0, rj.cpu_seconds or 0))
            self._drop(rj)
            any_finished = True
            if action == "complete":
                self.completed += 1
                self._event(f"job {rj.id} finished in {elapsed:.1f}s")
            else:
                self.failed += 1
                self._event(f"job {rj.id} did not succeed: {body['error']}")
            try:
                self._report(rj.id, action, body)
            except ControllerError:
                pass
        return any_finished

    def _drop(self, rj: RunningJob) -> None:
        self.running.pop(rj.id, None)
        release(rj.handle)

    # ------------------------------------------------------------ shutdown / status / control
    def _graceful_shutdown(self) -> None:
        self.state = "stopping"
        if self._revoked:
            for rj in list(self.running.values()):
                self._kill(rj)
                self._drop(rj)
        if self.running:
            self._event(f"waiting up to {self.cfg.shutdown_grace_s:.0f}s for {len(self.running)} running job(s)")
        deadline = time.monotonic() + (0 if self._immediate_stop else self.cfg.shutdown_grace_s)
        while self.running and time.monotonic() < deadline:
            try:
                self._poll_running()
            except ControllerError:
                break
            time.sleep(0.25)
        for rj in list(self.running.values()):
            self._kill(rj)
            self._drop(rj)
            try:
                self._call(f"/worker/jobs/{rj.id}/release", {"attempt": rj.attempt})
                self._event(f"returned unfinished job {rj.id} to the queue")
            except Exception:  # noqa: BLE001 - the lease will expire and the job is re-queued anyway
                pass
        try:
            self._flush_pending()
        except Exception:  # noqa: BLE001
            pass

    def _send_leave(self) -> None:
        try:
            self._call("/worker/leave", {})
            self._event("left the requester's group")
        except Exception:  # noqa: BLE001 - the token is forgotten here either way
            self._event("could not reach the requester to say goodbye; its owner can revoke this helper")
        self.cfg.token = self.cfg.worker_id = ""
        try:
            save_config(self.cfg, self.dir / "worker.json")
        except OSError:
            pass

    def _write_status(self) -> None:
        try:
            snap = self.snapshot() | {"pid": os.getpid(), "updated": time.time(), "version": __version__}
            (self.dir / "status.json").write_text(json.dumps(snap), encoding="utf-8")
        except OSError:
            pass

    def _read_control_file(self) -> None:
        """Lets `nc worker pause|resume|stop` talk to a worker running in another window."""
        path = self.dir / "control.txt"
        if not path.is_file():
            return
        try:
            commands = path.read_text(encoding="utf-8").split()
            path.unlink()
        except OSError:
            return
        for cmd in commands:
            if cmd == "pause":
                self.pause()
            elif cmd == "resume":
                self.resume()
            elif cmd == "stop":
                self._event("stop requested")
                self.stop_helping()
                self.stop_event.set()

    def _event(self, msg: str) -> None:
        log.info(msg)
        self.events.append(time.strftime("%H:%M:%S ") + msg)
        self.event_count += 1
