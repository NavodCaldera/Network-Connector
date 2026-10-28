"""The controller: FastAPI service (HTTPS, LAN-only) + SQLite state + lease reaper."""
from __future__ import annotations

import base64
import binascii
import json
import ipaddress
import logging
import math
import threading
import time
from collections import defaultdict, deque
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import uvicorn
import psutil
from fastapi import Depends, FastAPI, Header, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel, Field, field_validator

from . import __version__, jobs, modules
from .config import ControllerConfig, detect_lan_ip, load_config, save_config, validate_bind_host
from .paths import controller_dir
from .security import (
    constant_time_equal,
    fingerprint_pem_file,
    generate_self_signed,
    hash_token,
    load_or_create_admin_token,
    new_pairing_code,
    new_token,
    normalize_code,
    pairing_proof,
)
from .store import Store
from .wifidirect import DirectGroup, DirectLinkError, find_direct_interface
from .benchmark import BenchmarkRunner, BenchmarkSpec, shared_result

log = logging.getLogger("network_connector.controller")

MAX_RESULT_BYTES = 1_000_000
NAME_PATTERN = r"^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$"


# ------------------------------------------------------------------ errors
class ApiError(Exception):
    def __init__(self, status: int, code: str, message: str) -> None:
        self.status, self.code, self.message = status, code, message


# ------------------------------------------------------------------ request models
class PairRequest(BaseModel):
    worker_name: str = Field(pattern=NAME_PATTERN)
    nonce: str = Field(pattern=r"^[0-9a-f]{16,64}$")
    proof: str = Field(pattern=r"^[0-9a-f]{64}$")
    info: dict[str, Any] = Field(default_factory=dict)


class JobReport(BaseModel):
    id: str = Field(max_length=32)
    attempt: int = Field(ge=1)
    progress: float = 0


class HeartbeatRequest(BaseModel):
    info: dict[str, Any] | None = None
    metrics: dict[str, Any] = Field(default_factory=dict)
    version: str = Field(default="", max_length=32)
    jobs: list[JobReport] = Field(default_factory=list, max_length=64)

    @field_validator("metrics")
    @classmethod
    def valid_metrics(cls, metrics: dict[str, Any]) -> dict[str, Any]:
        bounds = {"cpu_percent": 100, "mem_percent": 100, "battery_percent": 100, "running_jobs": 64, "rtt_ms": 1e9, "cpu_freq_mhz": 1e6}
        for name, maximum in bounds.items():
            value = metrics.get(name)
            if value is not None and (isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 <= value <= maximum):
                raise ValueError(f"{name} must be a finite number between 0 and {maximum}")
        power = metrics.get("on_ac_power")
        if power is not None and not isinstance(power, bool):
            raise ValueError("on_ac_power must be a boolean or null")
        return metrics


class ClaimRequest(BaseModel):
    max_jobs: int = Field(default=1, ge=1, le=16)
    # Kinds this worker can run now. None = an older worker, which is never given wasm jobs.
    kinds: list[str] | None = Field(default=None, max_length=32)

    @field_validator("kinds")
    @classmethod
    def valid_kinds(cls, kinds: list[str] | None) -> list[str] | None:
        if kinds is not None and any(not isinstance(k, str) or len(k) > 40 for k in kinds):
            raise ValueError("kinds must be short job kind names")
        return kinds


class ModuleUpload(BaseModel):
    module_b64: str = Field(min_length=8, max_length=(modules.MAX_MODULE_BYTES + 2) // 3 * 4, repr=False)
    label: str | None = Field(default=None, max_length=120)


class CompleteRequest(BaseModel):
    attempt: int = Field(ge=1)
    result: dict[str, Any]
    duration: float = Field(ge=0, le=1e6)
    cpu_seconds: float | None = Field(default=None, ge=0, le=86400, allow_inf_nan=False)
    peak_mem_mb: float | None = Field(default=None, ge=0, le=1_048_576, allow_inf_nan=False)


class FailRequest(BaseModel):
    attempt: int = Field(ge=1)
    error: str = Field(max_length=2000)
    retryable: bool = True
    cancelled: bool = False
    duration: float = Field(default=0, ge=0, le=1e6)
    cpu_seconds: float | None = Field(default=None, ge=0, le=86400, allow_inf_nan=False)
    peak_mem_mb: float | None = Field(default=None, ge=0, le=1_048_576, allow_inf_nan=False)


class ReleaseRequest(BaseModel):
    attempt: int = Field(ge=1)


class DirectStartRequest(BaseModel):
    ssid: str | None = Field(default=None, min_length=1, max_length=32)
    passphrase: str | None = Field(default=None, min_length=8, max_length=63, repr=False)
    listen: bool = True


class DirectListenRequest(BaseModel):
    enabled: bool


class SubmitRequest(BaseModel):
    kind: str = Field(max_length=40)
    params: dict[str, Any] = Field(default_factory=dict)
    chunks: int = Field(default=1, ge=1, le=500)
    label: str | None = Field(default=None, max_length=120)
    idempotency_key: str | None = Field(default=None, max_length=120)
    timeout_s: int | None = Field(default=None, ge=1, le=86_400)
    max_attempts: int | None = Field(default=None, ge=1, le=5)
    target: str | None = Field(default=None, pattern=NAME_PATTERN)


class BatchItem(BaseModel):
    kind: str = Field(max_length=40)
    params: dict[str, Any]


class BatchSubmitRequest(BaseModel):
    jobs: list[BatchItem] = Field(min_length=1, max_length=500)
    label: str | None = Field(default=None, max_length=120)
    idempotency_key: str | None = Field(default=None, max_length=120)
    timeout_s: int | None = Field(default=None, ge=1, le=86_400)
    max_attempts: int | None = Field(default=None, ge=1, le=5)
    target: str | None = Field(default=None, pattern=NAME_PATTERN)


# ------------------------------------------------------------------ pairing
class PairingManager:
    """One active, single-use, expiring code held in memory only (never written to disk)."""

    def __init__(self, ttl_s: int, max_attempts: int = 5) -> None:
        self.ttl_s, self.max_attempts = ttl_s, max_attempts
        self._code: str | None = None
        self._expires = 0.0
        self._attempts = 0
        self._lock = threading.Lock()

    def create(self) -> tuple[str, int]:
        with self._lock:
            self._code, self._expires, self._attempts = new_pairing_code(), time.time() + self.ttl_s, 0
            return self._code, self.ttl_s

    def active(self) -> bool:
        with self._lock:
            return bool(self._code) and time.time() < self._expires and self._attempts < self.max_attempts

    def verify(self, proof: str, nonce: str, fingerprint: str) -> str | None:
        """Return the code on success (and burn it); None on a bad proof. Raises ApiError if inactive."""
        with self._lock:
            if not self._code or time.time() >= self._expires or self._attempts >= self.max_attempts:
                raise ApiError(403, "pairing_inactive", "No valid pairing code: it expired, was used, or was locked.")
            expected = pairing_proof(self._code, "worker", fingerprint, nonce)
            if not constant_time_equal(expected, proof):
                self._attempts += 1
                left = self.max_attempts - self._attempts
                log.warning("failed pairing attempt (%d attempts left)", left)
                return None
            code, self._code = self._code, None
            return code


# ------------------------------------------------------------------ shared state
@dataclass
class ControllerState:
    cfg: ControllerConfig
    store: Store
    fingerprint: str
    admin_token: str
    admin_hosts: set[str]
    pairing: PairingManager
    shutdown_cb: Any = None
    history: dict[str, deque] = field(default_factory=lambda: defaultdict(lambda: deque(maxlen=90)))
    benchmarks: Any = None
    direct_runtime: Any = None
    direct_subnets: set[str] = field(default_factory=set)
    worker_links: dict[str, str] = field(default_factory=dict)
    # "closed" (paired, trusted helpers: every kind) or "open" (anyone nearby: wasm only). The Link agent sets
    # this from the join policy, and may set group_mode_for(worker) -> mode to decide per worker.
    group_mode: str = "closed"
    group_mode_for: Any = None


def create_app(state: ControllerState) -> FastAPI:
    app = FastAPI(title="Network Connector Controller", version=__version__, docs_url=None, redoc_url=None, openapi_url=None)
    store, cfg = state.store, state.cfg
    state.benchmarks = BenchmarkRunner(store, cfg.worker_offline_after_s)

    @app.exception_handler(ApiError)
    async def _api_error(_: Request, exc: ApiError) -> JSONResponse:
        return JSONResponse({"error": {"code": exc.code, "message": exc.message}}, status_code=exc.status)

    @app.exception_handler(RequestValidationError)
    async def _validation(_: Request, exc: RequestValidationError) -> JSONResponse:
        first = exc.errors()[0] if exc.errors() else {}
        where = ".".join(str(p) for p in first.get("loc", []) if p != "body")
        return JSONResponse(
            {"error": {"code": "invalid_request", "message": f"{where}: {first.get('msg', 'invalid')}"}}, status_code=422
        )

    @app.middleware("http")
    async def _limit_body(request: Request, call_next):  # type: ignore[no-untyped-def]
        length = request.headers.get("content-length")
        if request.method in ("POST", "PUT") and (length is None or int(length) > cfg.max_request_bytes):
            return JSONResponse(
                {"error": {"code": "payload_too_large", "message": "Request body missing length or too large."}},
                status_code=413,
            )
        return await call_next(request)

    # ---- auth dependencies
    def require_admin(request: Request, x_admin_token: str | None = Header(default=None)) -> None:
        host = request.client.host if request.client else ""
        if host not in state.admin_hosts:
            raise ApiError(403, "admin_local_only", "Admin actions are only accepted from the controller computer itself.")
        if not x_admin_token or not constant_time_equal(x_admin_token, state.admin_token):
            raise ApiError(401, "bad_admin_token", "Missing or wrong admin token.")

    def require_worker(authorization: str | None = Header(default=None)) -> dict[str, Any]:
        if not authorization or not authorization.startswith("Bearer "):
            raise ApiError(401, "token_required", "Worker token required.")
        worker = store.worker_by_token(hash_token(authorization[7:]))
        if worker is None:
            raise ApiError(401, "unknown_or_revoked", "Unknown or revoked worker. Pair this worker again.")
        return worker

    # ---- public
    @app.get("/health")
    def health() -> dict[str, Any]:
        return {"status": "ok", "version": __version__}

    @app.post("/pair")
    def pair(req: PairRequest) -> dict[str, Any]:
        code = state.pairing.verify(req.proof, req.nonce, state.fingerprint)
        if code is None:
            raise ApiError(403, "bad_pairing_proof", "Pairing code incorrect, or the connection is not to the real controller.")
        token = new_token()
        wid = store.add_worker(req.worker_name, hash_token(token), req.info)
        log.info("paired worker %s (%s)", req.worker_name, wid)
        return {
            "worker_id": wid,
            "token": token,
            "controller_proof": pairing_proof(code, "controller", state.fingerprint, req.nonce),
        }

    # ---- worker API
    @app.post("/worker/heartbeat")
    def heartbeat(req: HeartbeatRequest, request: Request, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        link = "lan"
        try:
            source = ipaddress.ip_address(request.client.host if request.client else "")
            if source.is_loopback:
                link = "loopback"
            elif any(source in ipaddress.ip_network(network) for network in state.direct_subnets):
                link = "wifi-direct"
        except ValueError:
            pass
        state.worker_links[worker["id"]] = link
        req.metrics["link"] = link
        store.touch_worker(worker["id"], req.info, req.metrics, req.version)
        state.history[worker["id"]].append(
            (round(time.time(), 1), req.metrics.get("cpu_percent", 0), req.metrics.get("mem_percent", 0)))
        cancel, abandon = store.heartbeat_jobs(
            worker["id"], [r.model_dump() for r in req.jobs], cfg.lease_seconds)
        return {
            "cancel": cancel,
            "abandon": abandon,
            "paused": worker["state"] == "paused",
            "disabled": worker["state"] == "disabled",
            "link": link,
            "group_mode": worker_group_mode(worker),  # a helper may only tighten its own mode with this
        }

    def worker_group_mode(worker: dict[str, Any]) -> str:
        mode = state.group_mode_for(worker) if state.group_mode_for else state.group_mode
        return mode if mode in jobs.GROUP_MODES else "open"  # unknown means the strictest

    @app.post("/worker/claim")
    def claim(req: ClaimRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        if worker["state"] != "active":
            return {"jobs": []}
        offered = set(req.kinds) if req.kinds is not None else jobs.ALL_KINDS - {jobs.WASM}
        kinds = sorted(jobs.allowed_kinds(worker_group_mode(worker)) & offered)
        if not kinds:
            return {"jobs": []}
        claimed = store.claim(worker, req.max_jobs, cfg.lease_seconds, kinds)
        return {"jobs": [
            {"id": j["id"], "kind": j["kind"], "params": j["params"], "attempt": j["attempt"],
             "timeout_s": j["timeout_s"]} for j in claimed]}

    @app.post("/worker/jobs/{job_id}/complete")
    def complete(job_id: str, req: CompleteRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        job = store.get_job(job_id)
        limit = jobs.WASM_MAX_RESULT_BYTES if job and job["kind"] == jobs.WASM else MAX_RESULT_BYTES
        if len(json.dumps(req.result)) > limit:
            status = store.finish(job_id, worker["id"], req.attempt, error="result too large", retryable=False,
                                  duration=req.duration, cpu_seconds=req.cpu_seconds, peak_mem_mb=req.peak_mem_mb)
        else:
            status = store.finish(job_id, worker["id"], req.attempt, result=req.result, duration=req.duration, cpu_seconds=req.cpu_seconds, peak_mem_mb=req.peak_mem_mb)
        if status is None:
            raise ApiError(409, "stale_report", "This job is no longer assigned to this worker attempt.")
        return {"status": status}

    @app.post("/worker/jobs/{job_id}/fail")
    def fail(job_id: str, req: FailRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        status = store.finish(job_id, worker["id"], req.attempt, error=req.error, retryable=req.retryable,
                              cancelled=req.cancelled, duration=req.duration, cpu_seconds=req.cpu_seconds, peak_mem_mb=req.peak_mem_mb)
        if status is None:
            raise ApiError(409, "stale_report", "This job is no longer assigned to this worker attempt.")
        return {"status": status}

    @app.get("/worker/modules/{sha256}")
    def worker_module(sha256: str, worker: dict = Depends(require_worker)) -> Response:
        # Only while this worker runs a wasm job that uses the module; otherwise it does not exist for it.
        if not modules.SHA256_RE.match(sha256) or not store.worker_may_fetch_module(worker["id"], sha256):
            raise ApiError(404, "unknown_module", "No such module for a job you are running.")
        data = store.module_data(sha256)
        if data is None:
            raise ApiError(404, "unknown_module", "No such module for a job you are running.")
        return Response(content=data, media_type="application/wasm", headers={"X-Module-SHA256": sha256})

    @app.post("/worker/jobs/{job_id}/release")
    def release(job_id: str, req: ReleaseRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        return {"released": store.release(job_id, worker["id"], req.attempt)}

    # ---- admin API
    def direct_runtime() -> Any:
        if state.direct_runtime is None:
            raise ApiError(503, "direct_unavailable", "Wi-Fi Direct control requires a running controller runtime.")
        return state.direct_runtime

    @app.get("/admin/wifidirect", dependencies=[Depends(require_admin)])
    def admin_direct_status() -> dict[str, Any]:
        try:
            return direct_runtime().direct_status()
        except (DirectLinkError, ValueError, RuntimeError, OSError) as exc:
            raise ApiError(503, "direct_status_unavailable", str(exc)) from exc

    @app.post("/admin/wifidirect", dependencies=[Depends(require_admin)])
    def admin_direct_start(req: DirectStartRequest) -> dict[str, Any]:
        try:
            return direct_runtime().start_direct_group(req.ssid, req.passphrase, req.listen)
        except (DirectLinkError, ValueError, RuntimeError, OSError) as exc:
            raise ApiError(422, "direct_link_failed", str(exc)) from exc

    @app.post("/admin/wifidirect/stop", dependencies=[Depends(require_admin)])
    def admin_direct_stop() -> dict[str, Any]:
        try:
            return direct_runtime().stop_direct_group()
        except (DirectLinkError, RuntimeError, OSError) as exc:
            raise ApiError(422, "direct_link_failed", str(exc)) from exc

    @app.post("/admin/wifidirect/listen", dependencies=[Depends(require_admin)])
    def admin_direct_listen(req: DirectListenRequest) -> dict[str, Any]:
        try:
            return direct_runtime().set_direct_listener(req.enabled)
        except (DirectLinkError, ValueError, RuntimeError, OSError) as exc:
            raise ApiError(422, "direct_link_failed", str(exc)) from exc

    @app.get("/worker/benchmarks/latest")
    def worker_benchmark(worker: dict = Depends(require_worker)) -> dict[str, Any]:
        return {"benchmark": shared_result(store, worker["name"])}

    @app.post("/admin/jobs/cancel-all", dependencies=[Depends(require_admin)])
    def admin_cancel_all() -> dict[str, int]:
        return store.cancel_all()

    @app.post("/admin/benchmarks", dependencies=[Depends(require_admin)])
    def admin_start_benchmark(req: BenchmarkSpec) -> dict[str, Any]:
        try:
            return {"id": state.benchmarks.start(req)}
        except ValueError as exc:
            raise ApiError(422, "invalid_benchmark", str(exc)) from exc

    @app.get("/admin/benchmarks", dependencies=[Depends(require_admin)])
    def admin_benchmarks(limit: int = 50) -> dict[str, Any]:
        return {"benchmarks": store.benchmarks(min(200, max(1, limit)))}

    @app.get("/admin/benchmarks/{ident}", dependencies=[Depends(require_admin)])
    def admin_benchmark(ident: str) -> dict[str, Any]:
        result = store.benchmark(ident)
        if result is None:
            raise ApiError(404, "unknown_benchmark", "No benchmark with that ID.")
        return result

    @app.post("/admin/benchmarks/{ident}/cancel", dependencies=[Depends(require_admin)])
    def admin_cancel_benchmark(ident: str) -> dict[str, Any]:
        if store.benchmark(ident) is None:
            raise ApiError(404, "unknown_benchmark", "No benchmark with that ID.")
        return {"cancelling": state.benchmarks.cancel(ident)}

    @app.delete("/admin/benchmarks/{ident}", dependencies=[Depends(require_admin)])
    def admin_delete_benchmark(ident: str) -> dict[str, Any]:
        if not store.delete_benchmark(ident):
            raise ApiError(409, "not_deletable", "Benchmark is running or does not exist.")
        return {"deleted": True}

    def _check_job(kind: str, params: dict[str, Any]) -> None:
        try:
            clean = jobs.validate(kind, params)
        except jobs.JobError as exc:
            raise ApiError(422, "invalid_job", str(exc)) from exc
        if kind == jobs.WASM and not store.module_exists(clean["module_sha256"]):
            raise ApiError(422, "unknown_module", "Upload the module first (POST /admin/modules), then use its sha256.")

    @app.post("/admin/modules", dependencies=[Depends(require_admin)])
    def admin_upload_module(req: ModuleUpload) -> dict[str, Any]:
        try:
            data = base64.b64decode(req.module_b64, validate=True)
            digest = modules.check(data)
            modules.validate_with_wasmtime(data)
        except (binascii.Error, ValueError) as exc:  # ModuleError is a ValueError
            raise ApiError(422, "invalid_module", str(exc)) from exc
        created = store.add_module(digest, data, req.label)
        log.info("module %s uploaded (%d bytes)", digest[:12], len(data))
        return {"sha256": digest, "size": len(data), "created": created}

    @app.get("/admin/modules", dependencies=[Depends(require_admin)])
    def admin_modules() -> dict[str, Any]:
        return {"modules": store.list_modules()}

    def _defaults(timeout_s: int | None, max_attempts: int | None) -> tuple[int, int]:
        return timeout_s or cfg.default_timeout_s, max_attempts or cfg.default_max_attempts

    def _check_target(target: str | None) -> None:
        if target and store.get_worker(target) is None:
            raise ApiError(422, "unknown_target", f"No worker named {target!r}.")

    @app.post("/admin/pairing", dependencies=[Depends(require_admin)])
    def admin_pairing() -> dict[str, Any]:
        code, ttl = state.pairing.create()
        return {"code": code, "expires_in_seconds": ttl, "fingerprint": state.fingerprint}

    @app.get("/admin/summary", dependencies=[Depends(require_admin)])
    def admin_summary() -> dict[str, Any]:
        workers = store.list_workers()
        now = time.time()
        online = sum(1 for w in workers if w["state"] != "revoked" and w["last_seen"] and now - w["last_seen"] < cfg.worker_offline_after_s)
        return {"version": __version__, "workers_total": len(workers), "workers_online": online,
                "jobs": store.summary(), "pairing_active": state.pairing.active()}

    @app.get("/admin/workers", dependencies=[Depends(require_admin)])
    def admin_workers() -> dict[str, Any]:
        now = time.time()
        out = []
        for w in store.list_workers():
            age = (now - w["last_seen"]) if w["last_seen"] else None
            if w["state"] in ("revoked", "disabled", "paused"):
                status = w["state"]
            else:
                status = "online" if age is not None and age < cfg.worker_offline_after_s else "offline"
            w["status"], w["seconds_since_heartbeat"] = status, None if age is None else round(age, 1)
            w["history"] = list(state.history.get(w["id"], []))
            w["link"] = state.worker_links.get(w["id"], "unknown")
            out.append(w)
        return {"workers": out}

    @app.post("/admin/workers/{ident}/{action}", dependencies=[Depends(require_admin)])
    def admin_worker_action(ident: str, action: str) -> dict[str, Any]:
        new_state = {"pause": "paused", "resume": "active", "disable": "disabled", "enable": "active", "revoke": "revoked"}.get(action)
        if new_state is None:
            raise ApiError(404, "unknown_action", "Action must be pause, resume, disable, enable or revoke.")
        if not store.set_worker_state(ident, new_state):
            raise ApiError(404, "unknown_worker", f"No worker {ident!r}.")
        log.info("worker %s -> %s", ident, new_state)
        return {"worker": ident, "state": new_state}

    @app.post("/admin/jobs", dependencies=[Depends(require_admin)])
    def admin_submit(req: SubmitRequest) -> dict[str, Any]:
        _check_target(req.target)
        try:
            chunk_params = jobs.split(req.kind, req.params, req.chunks)
        except jobs.JobError as exc:
            raise ApiError(422, "invalid_job", str(exc)) from exc
        _check_job(req.kind, chunk_params[0])
        timeout_s, attempts = _defaults(req.timeout_s, req.max_attempts)
        bid, created = store.create_batch(
            req.kind, req.label, req.idempotency_key, [(req.kind, p) for p in chunk_params],
            timeout_s, attempts, req.target)
        return {"batch_id": bid, "created": created, "chunks": len(chunk_params)}

    @app.post("/admin/jobs/batch", dependencies=[Depends(require_admin)])
    def admin_submit_batch(req: BatchSubmitRequest) -> dict[str, Any]:
        _check_target(req.target)
        items = []
        for item in req.jobs:
            _check_job(item.kind, item.params)
            items.append((item.kind, jobs.validate(item.kind, item.params)))
        timeout_s, attempts = _defaults(req.timeout_s, req.max_attempts)
        kinds = {k for k, _ in items}
        bid, created = store.create_batch(
            kinds.pop() if len(kinds) == 1 else "mixed", req.label, req.idempotency_key, items,
            timeout_s, attempts, req.target)
        return {"batch_id": bid, "created": created, "chunks": len(items)}

    @app.get("/admin/jobs", dependencies=[Depends(require_admin)])
    def admin_jobs(status: str | None = None, batch_id: str | None = None, limit: int = 300) -> dict[str, Any]:
        return {"jobs": store.list_jobs(status, batch_id, min(max(limit, 1), 1000))}

    @app.get("/admin/jobs/{job_id}", dependencies=[Depends(require_admin)])
    def admin_job(job_id: str) -> dict[str, Any]:
        job = store.get_job(job_id)
        if job is None:
            raise ApiError(404, "unknown_job", f"No job {job_id!r}.")
        return job

    @app.post("/admin/jobs/{job_id}/cancel", dependencies=[Depends(require_admin)])
    def admin_cancel(job_id: str) -> dict[str, Any]:
        status = store.cancel_job(job_id)
        if status is None:
            raise ApiError(404, "unknown_job", f"No job {job_id!r}.")
        return {"status": status}

    @app.post("/admin/jobs/{job_id}/retry", dependencies=[Depends(require_admin)])
    def admin_retry(job_id: str) -> dict[str, Any]:
        if not store.retry_job(job_id):
            raise ApiError(409, "not_retryable", "Only failed or cancelled jobs can be retried.")
        return {"status": "queued"}

    @app.get("/admin/batches", dependencies=[Depends(require_admin)])
    def admin_batches(limit: int = 50) -> dict[str, Any]:
        return {"batches": store.list_batches(min(max(limit, 1), 200))}

    @app.get("/admin/batches/{batch_id}", dependencies=[Depends(require_admin)])
    def admin_batch(batch_id: str) -> dict[str, Any]:
        batch = store.get_batch(batch_id)
        if batch is None:
            raise ApiError(404, "unknown_batch", f"No batch {batch_id!r}.")
        if batch["done"]:
            rows = store.batch_results(batch_id)
            if all(r["status"] == "succeeded" for r in rows):
                batch["combined"] = jobs.combine(batch["kind"], [r["result"] for r in rows])
        return batch

    @app.post("/admin/batches/{batch_id}/cancel", dependencies=[Depends(require_admin)])
    def admin_cancel_batch(batch_id: str) -> dict[str, Any]:
        return {"cancelled_or_cancelling": store.cancel_batch(batch_id)}

    @app.post("/admin/shutdown", dependencies=[Depends(require_admin)])
    def admin_shutdown() -> dict[str, Any]:
        if state.shutdown_cb:
            threading.Timer(0.3, state.shutdown_cb).start()
        return {"status": "shutting down"}

    return app


# ------------------------------------------------------------------ runtime
class _Server(uvicorn.Server):
    def install_signal_handlers(self) -> None:  # we run in a thread; the owner handles Ctrl+C
        pass


def port_in_use_message(port: int) -> str:
    try:
        for conn in psutil.net_connections(kind="tcp"):
            if conn.status == psutil.CONN_LISTEN and conn.laddr.port == port and conn.pid:
                try:
                    name = psutil.Process(conn.pid).name()
                except (psutil.Error, OSError):
                    name = "unknown"
                return f"Port {port} is used by another program (PID {conn.pid}, name {name})"
    except (psutil.Error, OSError):
        pass
    return f"Controller failed to start on port {port}; the address may be unavailable or the port in use."


class ControllerRuntime:
    """Owns the TLS server thread and the lease-reaper thread."""

    def __init__(self, cfg: ControllerConfig | None = None, home: Path | None = None) -> None:
        self.dir = home or controller_dir()
        self.dir.mkdir(parents=True, exist_ok=True)
        self.cfg = cfg or load_config(ControllerConfig, self.dir / "controller.json")
        if not self.cfg.bind_host:
            self.cfg.bind_host = detect_lan_ip()
        self.cfg.bind_host = validate_bind_host(self.cfg.bind_host)
        if not isinstance(self.cfg.bind_hosts, list) or len(self.cfg.bind_hosts) > 16:
            raise ValueError("bind_hosts must contain at most 16 explicit IP addresses.")
        self.cfg.bind_hosts = list(dict.fromkeys([
            self.cfg.bind_host, *[validate_bind_host(h) for h in self.cfg.bind_hosts], "127.0.0.1"]))
        if len(self.cfg.bind_hosts) > 16:
            raise ValueError("At most 16 controller listeners are allowed, including loopback.")
        self.cert, self.key = self.dir / "server.crt", self.dir / "server.key"
        if self.cert.is_file() != self.key.is_file():
            raise RuntimeError("The controller certificate or private key is missing. Restore the matching pair from backup; refusing to replace its identity.")
        if not (self.cert.is_file() and self.key.is_file()):
            generate_self_signed(self.cert, self.key, [self.cfg.bind_host, "127.0.0.1"])
            log.info("created new TLS certificate")
        self.admin_token = load_or_create_admin_token(self.dir / "admin.token")
        self.fingerprint = fingerprint_pem_file(self.cert)
        self.store = Store(self.dir / "state.db")
        self.stop_event = threading.Event()
        self.state = ControllerState(
            cfg=self.cfg, store=self.store, fingerprint=self.fingerprint, admin_token=self.admin_token,
            admin_hosts={"127.0.0.1", "::1", *self.cfg.bind_hosts}, pairing=PairingManager(self.cfg.pairing_ttl_s),
            shutdown_cb=self.stop,
            direct_runtime=self,
        )
        self.direct_group = DirectGroup()
        self._direct_listener: str | None = None
        self._listener_lock = threading.RLock()
        self._direct_lock = threading.RLock()
        self.app = create_app(self.state)
        self._server: _Server | None = None
        self._servers: list[_Server] = []
        self._listeners: dict[str, tuple[_Server, threading.Thread]] = {}
        self._starting = False
        self._threads: list[threading.Thread] = []
        save_config(self.cfg, self.dir / "controller.json")

    @property
    def running(self) -> bool:
        return bool(self._servers) and all(s.started and not s.should_exit for s in self._servers) and not self.stop_event.is_set()

    @property
    def listener_hosts(self) -> list[str]:
        with self._listener_lock:
            return list(self._listeners)

    def _sync_servers(self) -> None:
        self._servers = [s for s, _ in self._listeners.values()]
        self._server = self._servers[0] if self._servers else None

    def add_listener(self, host: str, persist: bool = True) -> None:
        host = validate_bind_host(host)
        with self._listener_lock:
            if self.stop_event.is_set():
                raise RuntimeError("The controller is stopping; cannot add a listener.")
            if not self._starting and "127.0.0.1" not in self._listeners:
                raise RuntimeError("Start the controller before adding another listener.")
            if host in self._listeners:
                return
            if len(self._listeners) >= 16:
                raise ValueError("At most 16 controller listeners are allowed.")
            config = uvicorn.Config(
                self.app, host=host, port=self.cfg.port, ssl_keyfile=str(self.key),
                ssl_certfile=str(self.cert), log_level="warning", access_log=False,
            )
            server = _Server(config)
            thread = threading.Thread(target=server.run, name=f"controller-http-{host}", daemon=True)
            thread.start()
            deadline = time.monotonic() + 15
            while not server.started and time.monotonic() < deadline:
                if not thread.is_alive():
                    raise RuntimeError(port_in_use_message(self.cfg.port))
                time.sleep(0.05)
            if not server.started:
                server.should_exit = True
                thread.join(timeout=5)
                raise RuntimeError(f"Controller listener startup timed out on {host}:{self.cfg.port}.")
            old_hosts = list(self.cfg.bind_hosts)
            self.cfg.bind_hosts = list(dict.fromkeys([*old_hosts, host]))
            try:
                if persist:
                    save_config(self.cfg, self.dir / "controller.json")
            except OSError:
                self.cfg.bind_hosts = old_hosts
                server.should_exit = True
                thread.join(timeout=10)
                raise
            self._listeners[host] = server, thread
            self._threads.append(thread)
            self.state.admin_hosts.add(host)
            self._sync_servers()

    def remove_listener(self, host: str, persist: bool = True) -> None:
        host = validate_bind_host(host)
        if host == "127.0.0.1":
            raise ValueError("The loopback listener is required and cannot be removed.")
        with self._listener_lock:
            listener = self._listeners.get(host)
            if listener is None:
                return
            old_primary, old_hosts = self.cfg.bind_host, self.cfg.bind_hosts
            self.cfg.bind_hosts = [h for h in old_hosts if h != host]
            if self.cfg.bind_host == host:
                self.cfg.bind_host = next((h for h in self.cfg.bind_hosts if h != "127.0.0.1"), "127.0.0.1")
            try:
                if persist:
                    save_config(self.cfg, self.dir / "controller.json")
            except OSError:
                self.cfg.bind_host, self.cfg.bind_hosts = old_primary, old_hosts
                raise
            server, thread = listener
            server.should_exit = True
            if thread is not threading.current_thread():
                thread.join(timeout=10)
            if thread.is_alive():
                raise RuntimeError(f"The listener on {host} has not stopped yet.")
            del self._listeners[host]
            self.state.admin_hosts.discard(host)
            self._sync_servers()

    def start(self) -> None:
        if self.running:
            return
        recovered = self.store.recover_on_start(self.cfg.lease_seconds)
        if recovered:
            log.info("recovered %d running job(s) from before the restart; waiting for workers to reconnect", recovered)
        self.stop_event.clear()
        self._listeners.clear()
        self._threads = []
        self._starting = True
        try:
            for host in list(self.cfg.bind_hosts):
                self.add_listener(host, persist=False)
        except Exception:
            self.stop()
            raise
        finally:
            self._starting = False
        reaper = threading.Thread(target=self._reap, name="controller-reaper", daemon=True)
        self._threads.append(reaper)
        reaper.start()
        self._refresh_direct_subnets()
        log.info("controller listening on https://%s:%d", self.cfg.bind_host, self.cfg.port)

    def _refresh_direct_subnets(self) -> None:
        try:
            iface = find_direct_interface()
            self.state.direct_subnets = {iface.subnet} if iface else set()
        except DirectLinkError:
            self.state.direct_subnets = set()

    def direct_status(self) -> dict[str, Any]:
        info = self.direct_group.status()
        self.state.direct_subnets = {info.subnet} if info.ip and info.subnet else set()
        return asdict(info) | {"fingerprint": self.fingerprint, "listeners": self.listener_hosts,
                               "listening": bool(info.ip and info.ip in self.listener_hosts)}

    def start_direct_group(self, ssid: str | None = None, passphrase: str | None = None, listen: bool = True) -> dict[str, Any]:
        with self._direct_lock:
            self.direct_group.start(ssid, passphrase)
            if listen:
                try:
                    return self.set_direct_listener(True)
                except Exception:
                    self.direct_group.stop()
                    raise
            self._refresh_direct_subnets()
            return self.direct_status()

    def set_direct_listener(self, enabled: bool) -> dict[str, Any]:
        with self._direct_lock:
            if enabled:
                info = self.direct_group.status()
                deadline = time.monotonic() + 10
                while info.running and not info.ip and time.monotonic() < deadline:
                    time.sleep(0.5)
                    info = self.direct_group.status()
                if not info.running or not info.ip:
                    raise DirectLinkError("The direct group has no active IPv4 address. Start the group or Windows Mobile Hotspot first.")
                if info.ip not in self.listener_hosts:
                    self.add_listener(info.ip, persist=False)
                    self._direct_listener = info.ip
                if info.subnet:
                    self.state.direct_subnets = {info.subnet}
            elif self._direct_listener:
                self.remove_listener(self._direct_listener, persist=False)
                self._direct_listener = None
            return self.direct_status()

    def stop_direct_group(self) -> dict[str, Any]:
        with self._direct_lock:
            self.set_direct_listener(False)
            self.direct_group.stop()
            self._refresh_direct_subnets()
            return self.direct_status()

    def _reap(self) -> None:
        while not self.stop_event.wait(5):
            try:
                n = self.store.expire_leases()
                if n:
                    log.warning("re-queued/failed %d job(s) with expired lease or timeout", n)
            except Exception:  # noqa: BLE001
                log.exception("reaper error")

    def stop(self) -> None:
        self.stop_event.set()
        benchmarks = getattr(self.state, "benchmarks", None)
        if benchmarks:
            benchmarks.stop()
        try:
            self.direct_group.stop()
        except DirectLinkError:
            log.warning("Wi-Fi Direct publisher has not stopped yet")
        for server in self._servers:
            server.should_exit = True
        for t in self._threads:
            if t is not threading.current_thread():
                t.join(timeout=10)
        log.info("controller stopped")

    def wait(self) -> None:
        try:
            while self.running:
                time.sleep(0.5)
        except KeyboardInterrupt:
            pass
        finally:
            self.stop()
