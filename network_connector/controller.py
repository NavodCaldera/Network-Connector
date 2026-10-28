"""The controller: FastAPI service (HTTPS, LAN-only) + SQLite state + lease reaper."""
from __future__ import annotations

import base64
import binascii
import json
import ipaddress
import logging
import math
import re
import secrets
import threading
import time
from collections import OrderedDict, defaultdict, deque
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Literal

import uvicorn
import psutil
from fastapi import Depends, FastAPI, Header, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel, Field, field_validator

from . import __version__, identity, jobs, modules
from .config import (
    ControllerConfig,
    detect_lan_ip,
    legacy_controller_config,
    load_config,
    save_config,
    validate_bind_host,
    validate_join_settings,
)
from .discovery import Announcer, SlidingWindow
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


class JoinRequest(BaseModel):
    challenge: str = Field(pattern=r"^[A-Za-z0-9_-]{43}$")
    name: str = Field(min_length=1, max_length=40)
    device_cert: str = Field(min_length=64, max_length=4096)
    signature: str = Field(min_length=8, max_length=128, pattern=r"^[A-Za-z0-9+/=]+$")
    info: dict[str, Any] = Field(default_factory=dict)
    policy_summary: dict[str, Any] = Field(default_factory=dict)

    @field_validator("info", "policy_summary")
    @classmethod
    def small(cls, value: dict[str, Any]) -> dict[str, Any]:
        if len(json.dumps(value)) > 8192:
            raise ValueError("must be at most 8 KB of JSON")
        return value


class VerifyFinishRequest(BaseModel):
    session: str = Field(pattern=r"^[0-9a-f]{32}$")
    nonce: str = Field(pattern=r"^[0-9a-f]{32}$")


class PolicyUpdate(BaseModel):
    join_policy: Literal["anyone", "trusted", "ask", "code"] | None = None
    announce: bool | None = None
    announce_label: str | None = Field(default=None, max_length=60)


class CloseRequest(BaseModel):
    closing: bool


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


# ------------------------------------------------------------------ joining (open groups)
SERVER_INFO_KEYS = ("device_id", "device_name", "group_mode")  # set by the requester, never by the helper
JOIN_ACTIONS = ("trust", "untrust", "block", "unblock", "accept", "decline")


def worker_group_mode(worker: dict[str, Any]) -> str:
    """'open' for helpers that joined through /join, 'closed' for paired ones (AGENT.md §4 contract)."""
    info = worker.get("info") or {}
    if isinstance(info, str):
        try:
            info = json.loads(info)
        except ValueError:
            info = {}
    return "open" if isinstance(info, dict) and info.get("group_mode") == "open" else "closed"


def joined_worker_name(name: str, device_id: str) -> str:
    """Server-chosen, stable worker name for a joined device, so it can never pick a paired worker's name."""
    slug = re.sub(r"[^A-Za-z0-9._-]+", "-", name).strip("-._")[:40] or "helper"
    if not slug[0].isalnum():
        slug = "helper-" + slug
    return f"{slug[:40]}-{device_id[:8]}"


class JoinManager:
    """In-memory join state: single-use challenges, Ask-me requests, approvals and Verify sessions."""

    def __init__(self, challenge_ttl_s: int = 60, max_challenges: int = 512) -> None:
        self.challenge_ttl_s, self.max_challenges = challenge_ttl_s, max_challenges
        self._challenges: OrderedDict[str, tuple[float, str]] = OrderedDict()
        self.requests: dict[str, dict[str, Any]] = {}
        self._approved: dict[str, float] = {}
        self._verify: OrderedDict[str, dict[str, Any]] = OrderedDict()
        self.per_source = SlidingWindow(limit=60, window_s=60)  # an Ask-me wait polls hello + join every 5 s
        self.overall = SlidingWindow(limit=600, window_s=60, max_keys=1)
        self._lock = threading.Lock()

    def check_rate(self, source: str) -> None:
        if not self.overall.allow("all") or not self.per_source.allow(source):
            raise ApiError(429, "rate_limited", "Too many join requests. Wait a minute and try again.")

    def new_challenge(self, source: str) -> str:
        challenge = secrets.token_urlsafe(32)
        with self._lock:
            self._challenges[challenge] = (time.time() + self.challenge_ttl_s, source)
            while len(self._challenges) > self.max_challenges:
                self._challenges.popitem(last=False)
        return challenge

    def take_challenge(self, challenge: str, source: str) -> bool:
        with self._lock:
            item = self._challenges.pop(challenge, None)
        return bool(item) and time.time() < item[0] and item[1] == source

    def ask(self, device_id: str, name: str, source: str) -> None:
        with self._lock:
            now = time.time()
            for key in [k for k, r in self.requests.items() if now - r["ts"] > 600]:
                del self.requests[key]
            if device_id in self.requests or len(self.requests) < 64:
                self.requests[device_id] = {"device_id": device_id, "short_id": identity.short_id(device_id),
                                            "name": name, "source": source, "ts": now}

    def decide(self, device_id: str, accept: bool) -> bool:
        with self._lock:
            found = self.requests.pop(device_id, None) is not None
            if accept and found:
                self._approved[device_id] = time.time() + 600
            elif not accept:
                self._approved.pop(device_id, None)
            return found

    def take_approval(self, device_id: str) -> bool:
        with self._lock:
            expires = self._approved.pop(device_id, 0.0)
        return time.time() < expires

    def pending(self) -> list[dict[str, Any]]:
        with self._lock:
            return sorted((dict(r) for r in self.requests.values()), key=lambda r: r["ts"])

    # ---- Verify: numeric comparison, the requester commits to its nonce first
    def verify_start(self, helper_id: str, requester_id: str, worker_name: str) -> tuple[str, str]:
        session, nonce = secrets.token_hex(16), identity.new_nonce()
        with self._lock:
            self._expire_verify()
            self._verify[session] = {"helper_id": helper_id, "requester_id": requester_id, "nonce": nonce,
                                     "worker": worker_name, "ts": time.time(), "code": None}
            while len(self._verify) > 64:
                self._verify.popitem(last=False)
        return session, identity.commitment(nonce, helper_id, requester_id)

    def verify_finish(self, session: str, helper_id: str, helper_nonce: str) -> str:
        with self._lock:
            self._expire_verify()
            item = self._verify.get(session)
            if not item or item["helper_id"] != helper_id or item["code"] is not None:
                raise ApiError(404, "unknown_verify_session", "Start Verify again.")
            item["code"] = identity.verify_code(helper_id, item["requester_id"], helper_nonce, item["nonce"])
            return item["nonce"]

    def verifications(self) -> list[dict[str, Any]]:
        with self._lock:
            self._expire_verify()
            return [{"device_id": v["helper_id"], "short_id": identity.short_id(v["helper_id"]), "worker": v["worker"],
                     "code": v["code"], "ts": v["ts"]} for v in self._verify.values() if v["code"]]

    def _expire_verify(self) -> None:
        now = time.time()
        for key in [k for k, v in self._verify.items() if now - v["ts"] > 300]:
            del self._verify[key]


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
    identity: Any = None  # the requester's device identity (its TLS certificate and key)
    join: JoinManager = field(default_factory=JoinManager)
    closing: bool = False
    save_cfg: Any = None
    on_policy_change: Any = None


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
        limit = 32 * 1024 if request.url.path.startswith("/join") else cfg.max_request_bytes
        if request.method in ("POST", "PUT") and (length is None or not length.isdigit() or int(length) > limit):
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

    # ---- open groups: join without a code (PLAN §16.1)
    def _source(request: Request) -> str:
        return request.client.host if request.client else ""

    def _requester_name() -> str:
        return state.identity.name if state.identity else "Network Connector requester"

    def _joins_enabled() -> None:
        if cfg.join_policy == "code":
            raise ApiError(403, "join_disabled", "This requester only accepts helpers with a pairing code.")

    @app.post("/join/hello")
    def join_hello(request: Request) -> dict[str, Any]:
        source = _source(request)
        state.join.check_rate(source)
        _joins_enabled()
        return {"challenge": state.join.new_challenge(source), "requester_device_id": state.fingerprint,
                "requester_name": _requester_name(), "open": not state.closing, "policy": cfg.join_policy,
                "expires_in_seconds": state.join.challenge_ttl_s}

    @app.post("/join")
    def join(req: JoinRequest, request: Request) -> Any:
        source = _source(request)
        state.join.check_rate(source)
        _joins_enabled()
        if state.closing:
            raise ApiError(403, "requester_closing", "This requester is not accepting helpers right now.")
        if not state.join.take_challenge(req.challenge, source):
            raise ApiError(403, "bad_challenge", "The join challenge is unknown, used or expired. Start again.")
        try:
            cert = identity.load_cert(req.device_cert)
            signature = base64.b64decode(req.signature, validate=True)
            name = identity.clean_name(req.name)
        except (ValueError, binascii.Error) as exc:
            raise ApiError(422, "bad_device_identity", "The device certificate, signature or name is malformed.") from exc
        device_id = identity.device_id_of(cert)
        if device_id == state.fingerprint:
            raise ApiError(422, "own_device", "A requester can't join itself.")
        if not identity.verify_signature(cert, identity.join_message(req.challenge, state.fingerprint, device_id), signature):
            log.warning("join refused: bad signature from %s", source)
            raise ApiError(403, "bad_signature", "The device signature does not match the challenge.")
        device = store.get_device(device_id)
        short = identity.short_id(device_id)
        if device and device["blocked"]:
            log.info("join refused: blocked device %s", short)
            raise ApiError(403, "device_blocked", "This device is blocked by the requester.")
        trusted = bool(device and device["trusted"])
        if cfg.join_policy == "trusted" and not trusted:
            raise ApiError(403, "not_trusted", "This requester only accepts trusted devices. Ask its owner to trust you.")
        if cfg.join_policy == "ask" and not trusted and not state.join.take_approval(device_id):
            store.upsert_device(device_id, name, req.device_cert)
            state.join.ask(device_id, name, source)
            return JSONResponse({"status": "pending", "retry_after_seconds": 5,
                                 "message": "Waiting for the requester's owner to accept."}, status_code=202)
        worker_name = joined_worker_name(name, device_id)
        existing = store.get_worker(worker_name)
        if existing and json.loads(existing["info"] or "{}").get("device_id") != device_id:
            raise ApiError(409, "name_taken", "Another worker already uses this name on the requester.")
        store.upsert_device(device_id, name, req.device_cert)
        info = {k: v for k, v in req.info.items() if k not in SERVER_INFO_KEYS}
        info |= {"device_id": device_id, "device_name": name, "group_mode": "open", "policy_summary": req.policy_summary}
        token = new_token()
        wid = store.add_worker(worker_name, hash_token(token), info)
        log.info("helper %s (%s) joined with policy %s", worker_name, short, cfg.join_policy)
        return {"status": "joined", "worker_id": wid, "worker_name": worker_name, "token": token,
                "requester_device_id": state.fingerprint, "requester_name": _requester_name(), "group_mode": "open"}

    @app.post("/worker/leave")
    def worker_leave(worker: dict = Depends(require_worker)) -> dict[str, Any]:
        store.set_worker_state(worker["id"], "revoked")  # drops the token and re-queues its running jobs
        log.info("worker %s left", worker["name"])
        return {"left": True}

    def _helper_device(worker: dict[str, Any]) -> str:
        device_id = json.loads(worker.get("info") or "{}").get("device_id", "")
        if not identity.DEVICE_ID_RE.match(str(device_id)):
            raise ApiError(409, "no_device_identity", "Verify needs a helper that joined with its device identity.")
        return device_id

    @app.post("/worker/verify/start")
    def verify_start(worker: dict = Depends(require_worker)) -> dict[str, Any]:
        session, commit = state.join.verify_start(_helper_device(worker), state.fingerprint, worker["name"])
        return {"session": session, "commitment": commit, "requester_device_id": state.fingerprint}

    @app.post("/worker/verify/finish")
    def verify_finish(req: VerifyFinishRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        return {"nonce": state.join.verify_finish(req.session, _helper_device(worker), req.nonce)}

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
        if req.info is not None:
            saved = json.loads(worker.get("info") or "{}")
            req.info = {k: v for k, v in req.info.items() if k not in SERVER_INFO_KEYS} | {
                k: saved[k] for k in SERVER_INFO_KEYS if k in saved}
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
            "closing": state.closing,
            "group_mode": worker_group_mode(worker),  # a helper may only tighten its own mode with this
        }

    @app.post("/worker/claim")
    def claim(req: ClaimRequest, worker: dict = Depends(require_worker)) -> dict[str, Any]:
        if worker["state"] != "active" or state.closing:
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

    # ---- admin: identity, join policy and devices
    @app.get("/admin/identity", dependencies=[Depends(require_admin)])
    def admin_identity() -> dict[str, Any]:
        return {"device_id": state.fingerprint, "short_id": identity.short_id(state.fingerprint), "name": _requester_name()}

    def _policy() -> dict[str, Any]:
        announcer = getattr(state.direct_runtime, "announcer", None)
        return {"join_policy": cfg.join_policy, "announce": cfg.announce, "announce_label": cfg.announce_label,
                "closing": state.closing, "announcing": bool(announcer and announcer.running),
                "group_mode": "closed" if cfg.join_policy == "code" else "open"}

    @app.get("/admin/policy", dependencies=[Depends(require_admin)])
    def admin_policy() -> dict[str, Any]:
        return _policy()

    @app.put("/admin/policy", dependencies=[Depends(require_admin)])
    def admin_set_policy(req: PolicyUpdate) -> dict[str, Any]:
        for key, value in req.model_dump(exclude_none=True).items():
            setattr(cfg, key, value)
        validate_join_settings(cfg)
        if state.save_cfg:
            state.save_cfg()
        if state.on_policy_change:
            state.on_policy_change()
        log.info("join policy is now %s (announce %s)", cfg.join_policy, cfg.announce)
        return _policy()

    @app.post("/admin/close", dependencies=[Depends(require_admin)])
    def admin_close(req: CloseRequest) -> dict[str, Any]:
        state.closing = req.closing
        return _policy()

    def _device_ref(ref: str) -> str:
        try:
            ref = identity.normalize_device_ref(ref)
        except ValueError as exc:
            raise ApiError(422, "bad_device_id", str(exc)) from exc
        if len(ref) == 64:
            return ref
        known = {d["device_id"] for d in store.list_devices()} | {r["device_id"] for r in state.join.pending()}
        matches = sorted(d for d in known if d.startswith(ref))
        if len(matches) != 1:
            raise ApiError(404, "unknown_device", "Unknown or ambiguous short ID. Use the full device ID.")
        return matches[0]

    def _workers_of(device_id: str) -> list[dict[str, Any]]:
        return [w for w in store.list_workers() if w["info"].get("device_id") == device_id]

    @app.get("/admin/devices", dependencies=[Depends(require_admin)])
    def admin_devices() -> dict[str, Any]:
        devices = []
        for d in store.list_devices():
            d.pop("cert", None)
            d["short_id"] = identity.short_id(d["device_id"])
            d["workers"] = [w["name"] for w in _workers_of(d["device_id"]) if w["state"] != "revoked"]
            devices.append(d)
        return {"devices": devices, "requests": state.join.pending(), "verifications": state.join.verifications()}

    @app.post("/admin/devices/{ref}/{action}", dependencies=[Depends(require_admin)])
    def admin_device_action(ref: str, action: str) -> dict[str, Any]:
        if action not in JOIN_ACTIONS:
            raise ApiError(404, "unknown_action", f"Action must be one of {', '.join(JOIN_ACTIONS)}.")
        device_id = _device_ref(ref)
        if action in ("trust", "untrust"):
            store.set_trusted(device_id, action == "trust")
        elif action in ("block", "unblock"):
            store.set_blocked(device_id, action == "block")
            if action == "block":
                store.set_trusted(device_id, False)
                state.join.decide(device_id, accept=False)
                for w in _workers_of(device_id):  # its tokens stop working at once
                    store.set_worker_state(w["id"], "revoked")
        elif not state.join.decide(device_id, accept=action == "accept"):
            raise ApiError(404, "no_request", "This device has no pending join request.")
        log.info("device %s -> %s", identity.short_id(device_id), action)
        return {"device_id": device_id, "action": action}

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
        if legacy_controller_config(self.dir / "controller.json"):
            # Upgraded from 0.2.0 or earlier: stay a closed group until the owner opens it.
            self.cfg.join_policy, self.cfg.announce = "code", False
        validate_join_settings(self.cfg)
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
            if home is None:
                # The requester's server certificate is the device certificate (PLAN §14).
                identity.install_server_cert(self.cert, self.key, identity.current())
                log.info("using the device identity as the TLS certificate")
            else:
                generate_self_signed(self.cert, self.key, [self.cfg.bind_host, "127.0.0.1"])
                log.info("created new TLS certificate")
        self.admin_token = load_or_create_admin_token(self.dir / "admin.token")
        self.fingerprint = fingerprint_pem_file(self.cert)
        # A 0.2.0 controller certificate is adopted as the device identity, so both are the same key.
        name = identity.current().name if home is None else identity.default_name()
        self.identity = identity.Identity.from_files(self.cert, self.key, name)
        if home is None and identity.current().device_id != self.fingerprint:
            log.warning("the controller certificate differs from the device identity; helpers see the controller certificate")
        self.announcer: Announcer | None = None
        self.store = Store(self.dir / "state.db")
        self.stop_event = threading.Event()
        self.state = ControllerState(
            cfg=self.cfg, store=self.store, fingerprint=self.fingerprint, admin_token=self.admin_token,
            admin_hosts={"127.0.0.1", "::1", *self.cfg.bind_hosts}, pairing=PairingManager(self.cfg.pairing_ttl_s),
            shutdown_cb=self.stop,
            direct_runtime=self,
            identity=self.identity,
            save_cfg=lambda: save_config(self.cfg, self.dir / "controller.json"),
            on_policy_change=self._sync_announcer,
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
        self.state.closing = False
        self._sync_announcer()
        log.info("controller listening on https://%s:%d", self.cfg.bind_host, self.cfg.port)

    # ---- nearby announcements (requester side)
    def _announce_state(self) -> dict[str, Any]:
        summary = self.store.summary()
        now = time.time()
        helpers = sum(1 for w in self.store.list_workers() if w["state"] == "active" and w["last_seen"]
                      and now - w["last_seen"] < self.cfg.worker_offline_after_s)
        return {"port": self.cfg.port, "open": self.cfg.join_policy != "code" and not self.state.closing,
                "threads": summary["queued"] + summary["running"], "helpers": helpers,
                "job_label": self.cfg.announce_label}

    def _sync_announcer(self) -> None:
        with self._listener_lock:
            wanted = self.cfg.announce and self.cfg.join_policy != "code" and not self.stop_event.is_set() and bool(self._servers)
            if wanted and self.announcer is None:
                self.announcer = Announcer(self.identity, lambda: self.listener_hosts, self._announce_state,
                                           dest_port=self.cfg.discovery_port)
                self.announcer.start()
            elif not wanted and self.announcer is not None:
                self.announcer.stop()
                self.announcer = None

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
        self.state.closing = True
        if self.announcer is not None:
            self.announcer.stop()
            self.announcer = None
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
