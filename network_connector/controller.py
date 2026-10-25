"""The controller: FastAPI service (HTTPS, LAN-only) + SQLite state + lease reaper."""
from __future__ import annotations

import json
import logging
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import uvicorn
from fastapi import Depends, FastAPI, Header, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field

from . import __version__, jobs
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


class ClaimRequest(BaseModel):
    max_jobs: int = Field(default=1, ge=1, le=16)


class CompleteRequest(BaseModel):
    attempt: int = Field(ge=1)
    result: dict[str, Any]
    duration: float = Field(ge=0, le=1e6)


class FailRequest(BaseModel):
    attempt: int = Field(ge=1)
    error: str = Field(max_length=2000)
    retryable: bool = True
    cancelled: bool = False
    duration: float = Field(default=0, ge=0, le=1e6)


class ReleaseRequest(BaseModel):
    attempt: int = Field(ge=1)


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


def create_app(state: ControllerState) -> FastAPI:
    app = FastAPI(title="Network Connector Controller", version=__version__, docs_url=None, redoc_url=None, openapi_url=None)
    store, cfg = state.store, state.cfg

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
    def heartbeat(req: HeartbeatRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
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
        }

    @app.post("/worker/claim")
    def claim(req: ClaimRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        if worker["state"] != "active":
            return {"jobs": []}
        claimed = store.claim(worker, req.max_jobs, cfg.lease_seconds)
        return {"jobs": [
            {"id": j["id"], "kind": j["kind"], "params": j["params"], "attempt": j["attempt"],
             "timeout_s": j["timeout_s"]} for j in claimed]}

    @app.post("/worker/jobs/{job_id}/complete")
    def complete(job_id: str, req: CompleteRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        if len(json.dumps(req.result)) > MAX_RESULT_BYTES:
            status = store.finish(job_id, worker["id"], req.attempt, error="result too large", retryable=False,
                                  duration=req.duration)
        else:
            status = store.finish(job_id, worker["id"], req.attempt, result=req.result, duration=req.duration)
        if status is None:
            raise ApiError(409, "stale_report", "This job is no longer assigned to this worker attempt.")
        return {"status": status}

    @app.post("/worker/jobs/{job_id}/fail")
    def fail(job_id: str, req: FailRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        status = store.finish(job_id, worker["id"], req.attempt, error=req.error, retryable=req.retryable,
                              cancelled=req.cancelled, duration=req.duration)
        if status is None:
            raise ApiError(409, "stale_report", "This job is no longer assigned to this worker attempt.")
        return {"status": status}

    @app.post("/worker/jobs/{job_id}/release")
    def release(job_id: str, req: ReleaseRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        return {"released": store.release(job_id, worker["id"], req.attempt)}

    # ---- admin API
    def _check_job(kind: str, params: dict[str, Any]) -> None:
        try:
            jobs.validate(kind, params)
        except jobs.JobError as exc:
            raise ApiError(422, "invalid_job", str(exc)) from exc

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


class ControllerRuntime:
    """Owns the TLS server thread and the lease-reaper thread."""

    def __init__(self, cfg: ControllerConfig | None = None, home: Path | None = None) -> None:
        self.dir = home or controller_dir()
        self.dir.mkdir(parents=True, exist_ok=True)
        self.cfg = cfg or load_config(ControllerConfig, self.dir / "controller.json")
        if not self.cfg.bind_host:
            self.cfg.bind_host = detect_lan_ip()
        self.cfg.bind_host = validate_bind_host(self.cfg.bind_host)
        self.cert, self.key = self.dir / "server.crt", self.dir / "server.key"
        if not (self.cert.is_file() and self.key.is_file()):
            generate_self_signed(self.cert, self.key, [self.cfg.bind_host, "127.0.0.1"])
            log.info("created new TLS certificate")
        self.admin_token = load_or_create_admin_token(self.dir / "admin.token")
        self.fingerprint = fingerprint_pem_file(self.cert)
        self.store = Store(self.dir / "state.db")
        self.stop_event = threading.Event()
        self.state = ControllerState(
            cfg=self.cfg, store=self.store, fingerprint=self.fingerprint, admin_token=self.admin_token,
            admin_hosts={"127.0.0.1", "::1", self.cfg.bind_host}, pairing=PairingManager(self.cfg.pairing_ttl_s),
            shutdown_cb=self.stop,
        )
        self.app = create_app(self.state)
        self._server: _Server | None = None
        self._threads: list[threading.Thread] = []
        save_config(self.cfg, self.dir / "controller.json")

    @property
    def running(self) -> bool:
        return bool(self._server and self._server.started and not self._server.should_exit)

    def start(self) -> None:
        recovered = self.store.recover_on_start(self.cfg.lease_seconds)
        if recovered:
            log.info("recovered %d running job(s) from before the restart; waiting for workers to reconnect", recovered)
        config = uvicorn.Config(
            self.app, host=self.cfg.bind_host, port=self.cfg.port, ssl_keyfile=str(self.key),
            ssl_certfile=str(self.cert), log_level="warning", access_log=False,
        )
        self._server = _Server(config)
        server_thread = threading.Thread(target=self._server.run, name="controller-http", daemon=True)
        reaper = threading.Thread(target=self._reap, name="controller-reaper", daemon=True)
        self._threads = [server_thread, reaper]
        self.stop_event.clear()
        for t in self._threads:
            t.start()
        deadline = time.time() + 15
        while not self._server.started and time.time() < deadline:
            if not server_thread.is_alive():
                raise RuntimeError(
                    f"Controller failed to start on {self.cfg.bind_host}:{self.cfg.port} (port already in use?)")
            time.sleep(0.05)
        log.info("controller listening on https://%s:%d", self.cfg.bind_host, self.cfg.port)

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
        if self._server:
            self._server.should_exit = True
        for t in self._threads:
            if t is not threading.current_thread():
                t.join(timeout=10)
        log.info("controller stopped")

    def wait(self) -> None:
        try:
            while self._server and not self._server.should_exit:
                time.sleep(0.5)
        except KeyboardInterrupt:
            pass
        finally:
            self.stop()
