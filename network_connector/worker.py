"""The worker agent: connects OUT to the controller, pulls jobs, runs them with resource limits.

The worker opens no listening port, so nothing on the network can connect to it.
"""
from __future__ import annotations

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

from . import __version__
from .client import ControllerError, controller_url, pinned_context, probe, raise_for_error
from .config import WorkerConfig, load_config, save_config
from .security import constant_time_equal, fingerprint_pem_file
from .sysinfo import hardware_info, live_metrics, prime_cpu_counter
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
    proc: subprocess.Popen
    handle: object | None
    started: float = field(default_factory=time.monotonic)
    progress: float = 0.0
    stdout: bytes = b""
    cancel: bool = False
    abandon: bool = False
    cpu_seconds: float | None = None
    peak_mem_mb: float = 0.0
    threads: list[threading.Thread] = field(default_factory=list)


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
        return {
            "state": self.state, "paused": self.local_paused or self.remote_paused, "disabled": self.remote_disabled,
            "running": [{"id": j.id, "kind": j.kind, "progress": round(j.progress, 1),
                         "elapsed": round(time.monotonic() - j.started, 1)} for j in self.running.values()],
            "completed": self.completed, "failed": self.failed, "last_error": self.last_error,
            "slots": self.slots, "metrics": live_metrics(), "name": self.cfg.name,
            "link": self._link, "controller_host": self.cfg.controller_host, "rtt_ms": self._rtt_ms,
        }

    # ------------------------------------------------------------ main loop
    def run(self) -> None:
        prime_cpu_counter()
        self._http = self._new_client(self.cfg.controller_host)
        self.state = "connecting"
        self._event(f"worker '{self.cfg.name}' starting; controller {self.cfg.controller_host}:{self.cfg.controller_port}")
        next_hb = next_claim = 0.0
        backoff_i = 0
        last_activity = time.monotonic()
        try:
            while not self.stop_event.is_set():
                self._read_control_file()
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
                self.stop_event.wait(0.25)
        finally:
            self._graceful_shutdown()
            if self._http:
                self._http.close()
            self.state = "revoked" if self._revoked else "stopped"
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
        self._write_status()

    def _can_claim(self) -> bool:
        if self.stop_event.is_set() or self.local_paused or self.remote_paused or self.remote_disabled:
            return False
        if len(self.running) >= self.slots or self.state in ("stopping", "offline", "connecting"):
            return False
        return psutil.virtual_memory().percent < 92  # never start work when the laptop is nearly out of RAM

    def _claim(self) -> bool:
        resp = self._call("/worker/claim", {"max_jobs": self.slots - len(self.running)})
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
    def _start_job(self, job: dict[str, Any]) -> None:
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
        rj = RunningJob(job["id"], job["kind"], job["attempt"], job["timeout_s"], proc,
                        limit_memory(proc.pid, self.cfg.max_memory_mb))
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

    def _kill(self, rj: RunningJob) -> None:
        try:
            rj.proc.kill()
            rj.proc.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            pass

    def _poll_running(self) -> bool:
        any_finished = False
        for rj in list(self.running.values()):
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
        deadline = time.monotonic() + self.cfg.shutdown_grace_s
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
                self.stop_event.set()

    def _event(self, msg: str) -> None:
        log.info(msg)
        self.events.append(time.strftime("%H:%M:%S ") + msg)
        self.event_count += 1
