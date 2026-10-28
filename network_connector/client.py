"""HTTPS clients that trust ONLY the controller's own certificate (certificate pinning)."""
from __future__ import annotations

import base64
import secrets
import ssl
import ipaddress
from pathlib import Path
from typing import Any

import httpx

from . import __version__
from .config import WorkerConfig, save_config
from .security import (
    constant_time_equal,
    fingerprint_der,
    normalize_code,
    pairing_proof,
    protect_path,
)
from .sysinfo import hardware_info


class ControllerError(RuntimeError):
    """The controller replied with a structured error."""

    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"{message} [{code}]")
        self.status, self.code, self.message = status, code, message


def pinned_context(cafile: Path) -> ssl.SSLContext:
    """Trust only this certificate. Hostname checking is off because identity is the pinned cert itself."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_REQUIRED
    ctx.load_verify_locations(cafile=str(cafile))
    return ctx


def controller_url(host: str, port: int) -> str:
    return f"https://{'[' + host + ']' if ':' in host else host}:{port}"


def raise_for_error(resp: httpx.Response) -> dict[str, Any]:
    try:
        body = resp.json()
    except ValueError:
        body = {}
    if resp.status_code >= 400:
        err = body.get("error", {}) if isinstance(body, dict) else {}
        raise ControllerError(resp.status_code, err.get("code", "http_error"), err.get("message", resp.text[:200]))
    return body


class AdminClient:
    """Used by the CLI and GUI on the controller computer."""

    def __init__(self, host: str, port: int, cert: Path, admin_token: str, timeout: float = 20) -> None:
        self._http = httpx.Client(
            base_url=controller_url(host, port), verify=pinned_context(cert),
            headers={"X-Admin-Token": admin_token}, timeout=timeout,
        )

    def close(self) -> None:
        self._http.close()

    def _call(self, method: str, path: str, **kw: Any) -> dict[str, Any]:
        return raise_for_error(self._http.request(method, path, **kw))

    def health(self) -> dict[str, Any]:
        return self._call("GET", "/health")

    def summary(self) -> dict[str, Any]:
        return self._call("GET", "/admin/summary")

    def create_pairing(self) -> dict[str, Any]:
        return self._call("POST", "/admin/pairing")

    def workers(self) -> list[dict[str, Any]]:
        return self._call("GET", "/admin/workers")["workers"]

    def worker_action(self, ident: str, action: str) -> dict[str, Any]:
        return self._call("POST", f"/admin/workers/{ident}/{action}")

    def submit(self, kind: str, params: dict[str, Any], **opts: Any) -> dict[str, Any]:
        return self._call("POST", "/admin/jobs", json={"kind": kind, "params": params, **opts})

    def submit_batch(self, jobs: list[dict[str, Any]], **opts: Any) -> dict[str, Any]:
        return self._call("POST", "/admin/jobs/batch", json={"jobs": jobs, **opts})

    def jobs(self, status: str | None = None, batch_id: str | None = None, limit: int = 300) -> list[dict[str, Any]]:
        params = {k: v for k, v in {"status": status, "batch_id": batch_id, "limit": limit}.items() if v}
        return self._call("GET", "/admin/jobs", params=params)["jobs"]

    def job(self, job_id: str) -> dict[str, Any]:
        return self._call("GET", f"/admin/jobs/{job_id}")

    def cancel_job(self, job_id: str) -> dict[str, Any]:
        return self._call("POST", f"/admin/jobs/{job_id}/cancel")

    def cancel_all(self) -> dict[str, Any]:
        return self._call("POST", "/admin/jobs/cancel-all")

    def start_benchmark(self, spec: dict[str, Any]) -> dict[str, Any]:
        return self._call("POST", "/admin/benchmarks", json=spec)

    def benchmarks(self, limit: int = 50) -> list[dict[str, Any]]:
        return self._call("GET", "/admin/benchmarks", params={"limit": limit})["benchmarks"]

    def benchmark(self, ident: str) -> dict[str, Any]:
        return self._call("GET", f"/admin/benchmarks/{ident}")

    def cancel_benchmark(self, ident: str) -> dict[str, Any]:
        return self._call("POST", f"/admin/benchmarks/{ident}/cancel")

    def delete_benchmark(self, ident: str) -> dict[str, Any]:
        return self._call("DELETE", f"/admin/benchmarks/{ident}")

    def upload_module(self, data: bytes, label: str | None = None) -> dict[str, Any]:
        """Upload a Wasm module (at most 10 MB). Returns {sha256, size, created}."""
        return self._call("POST", "/admin/modules", json={"module_b64": base64.b64encode(data).decode(), "label": label},
                          timeout=120)

    def modules(self) -> list[dict[str, Any]]:
        return self._call("GET", "/admin/modules")["modules"]

    def retry_job(self, job_id: str) -> dict[str, Any]:
        return self._call("POST", f"/admin/jobs/{job_id}/retry")

    def batches(self, limit: int = 50) -> list[dict[str, Any]]:
        return self._call("GET", "/admin/batches", params={"limit": limit})["batches"]

    def batch(self, batch_id: str) -> dict[str, Any]:
        return self._call("GET", f"/admin/batches/{batch_id}")

    def cancel_batch(self, batch_id: str) -> dict[str, Any]:
        return self._call("POST", f"/admin/batches/{batch_id}/cancel")

    def shutdown(self) -> dict[str, Any]:
        return self._call("POST", "/admin/shutdown")

    def direct_status(self) -> dict[str, Any]:
        return self._call("GET", "/admin/wifidirect")

    def direct_start(self, ssid: str | None = None, passphrase: str | None = None, listen: bool = True) -> dict[str, Any]:
        return self._call("POST", "/admin/wifidirect", json={"ssid": ssid, "passphrase": passphrase, "listen": listen}, timeout=45)

    def direct_stop(self) -> dict[str, Any]:
        return self._call("POST", "/admin/wifidirect/stop", timeout=30)

    def direct_listen(self, enabled: bool) -> dict[str, Any]:
        return self._call("POST", "/admin/wifidirect/listen", json={"enabled": enabled}, timeout=30)


def admin_client_from_disk(controller_dir_: Path) -> AdminClient:
    from .config import ControllerConfig, load_config

    cfg = load_config(ControllerConfig, controller_dir_ / "controller.json")
    if not cfg.bind_host:
        raise ControllerError(0, "not_started", "The controller has not been started yet.")
    token = (controller_dir_ / "admin.token").read_text(encoding="utf-8").strip()
    return AdminClient("127.0.0.1", cfg.port, controller_dir_ / "server.crt", token)


# ---------------------------------------------------------------- module download (worker side)
def fetch_module(http: httpx.Client, sha256: str, max_bytes: int) -> bytes:
    """Download a module for a running wasm job. The caller checks the hash before using it."""
    with http.stream("GET", f"/worker/modules/{sha256}", timeout=120) as resp:
        if resp.status_code >= 400:
            resp.read()
            raise_for_error(resp)
        data = bytearray()
        for chunk in resp.iter_bytes():
            data += chunk
            if len(data) > max_bytes:
                raise ControllerError(0, "module_too_large", "The module is larger than the 10 MB limit; refused.")
        return bytes(data)


# ---------------------------------------------------------------- pairing (worker side)
def latest_benchmark(cfg: WorkerConfig, wdir: Path) -> dict[str, Any] | None:
    with httpx.Client(base_url=controller_url(cfg.controller_host, cfg.controller_port), verify=pinned_context(wdir / "controller.crt"), headers={"Authorization": f"Bearer {cfg.token}"}, timeout=10) as http:
        return raise_for_error(http.get("/worker/benchmarks/latest"))["benchmark"]


def fetch_server_cert(host: str, port: int, timeout: float = 10) -> tuple[str, str]:
    """Return (PEM, fingerprint) of the certificate the server presents right now."""
    pem = ssl.get_server_certificate((host, port), timeout=timeout)  # type: ignore[call-arg]
    der = ssl.PEM_cert_to_DER_cert(pem)
    return pem, fingerprint_der(der)


def probe(host: str, port: int, timeout: float = 2) -> str | None:
    """Read a candidate's certificate without sending a token or trusting its identity."""
    try:
        ip = ipaddress.ip_address(host)
        if ip.is_unspecified or not (ip.is_private or ip.is_loopback or ip.is_link_local):
            return None
        if not isinstance(port, int) or not 1 <= port <= 65535:
            return None
        return fetch_server_cert(str(ip), port, timeout=max(0.1, min(timeout, 10)))[1]
    except (OSError, ValueError):
        return None


def pair_worker(host: str, port: int, code: str, name: str, worker_dir: Path, cfg: WorkerConfig) -> WorkerConfig:
    """Pair with the controller using a one-time code and store the result (token + pinned cert).

    The proof sent to the controller is bound to the certificate seen here, and the controller's
    reply is verified against the same certificate. A man-in-the-middle with a different
    certificate cannot satisfy either check without knowing the code.
    """
    if not normalize_code(code):
        raise ControllerError(0, "empty_code", "Enter the pairing code shown on the controller.")
    pem, fingerprint = fetch_server_cert(host, port)
    worker_dir.mkdir(parents=True, exist_ok=True)
    cert_path = worker_dir / "controller.crt"
    cert_path.write_text(pem, encoding="utf-8")
    nonce = secrets.token_hex(16)
    with httpx.Client(base_url=controller_url(host, port), verify=pinned_context(cert_path), timeout=20) as http:
        body = raise_for_error(http.post("/pair", json={
            "worker_name": name, "nonce": nonce,
            "proof": pairing_proof(code, "worker", fingerprint, nonce),
            "info": hardware_info() | {"client_version": __version__},
        }))
    expected = pairing_proof(code, "controller", fingerprint, nonce)
    if not constant_time_equal(expected, str(body.get("controller_proof", ""))):
        cert_path.unlink(missing_ok=True)
        raise ControllerError(0, "controller_not_authentic", "The controller failed to prove it knows the pairing code. Aborting.")
    cfg.controller_host, cfg.controller_port = host, port
    cfg.controller_fingerprint = fingerprint
    cfg.known_hosts = [host]
    cfg.name, cfg.worker_id, cfg.token = name, body["worker_id"], body["token"]
    save_config(cfg, worker_dir / "worker.json")
    protect_path(cert_path)
    return cfg


# ---------------------------------------------------------------- joining open groups (helper side, PLAN §16.1)
class JoinPending(ControllerError):
    """The requester's owner has not accepted this helper yet (its policy is Ask me)."""


def _checked_address(host: str, port: int) -> str:
    ip = ipaddress.ip_address(host)
    if ip.is_unspecified or ip.is_multicast or not (ip.is_private or ip.is_loopback or ip.is_link_local):
        raise ControllerError(0, "bad_address", "Only private LAN, link-local or loopback addresses can be joined.")
    if not isinstance(port, int) or not 1 <= port <= 65535:
        raise ControllerError(0, "bad_address", "The port must be between 1 and 65535.")
    return str(ip)


def _pem_context(pem: str) -> ssl.SSLContext:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_REQUIRED
    ctx.load_verify_locations(cadata=pem)
    return ctx


def _hello(http: httpx.Client, fingerprint: str) -> dict[str, Any]:
    body = raise_for_error(http.post("/join/hello", json={}))
    if not constant_time_equal(str(body.get("requester_device_id", "")), fingerprint):
        raise ControllerError(0, "wrong_requester", "The requester's answer does not match its certificate.")
    return body


def requester_hello(host: str, port: int, timeout: float = 3) -> dict[str, Any] | None:
    """Who is at this address? Outbound only, no token or identity is sent. None if it isn't an open requester."""
    try:
        host = _checked_address(host, port)
        pem, fingerprint = fetch_server_cert(host, port, timeout=max(0.2, min(timeout, 10)))
        with httpx.Client(base_url=controller_url(host, port), verify=_pem_context(pem), timeout=timeout) as http:
            body = _hello(http, fingerprint)
        from .identity import clean_name

        return {"device_id": fingerprint, "name": clean_name(str(body.get("requester_name", "requester"))),
                "open": bool(body.get("open")), "policy": str(body.get("policy", ""))}
    except (ControllerError, httpx.HTTPError, OSError, ValueError, ssl.SSLError):
        return None


def joined_dir(device_id: str) -> Path:
    """Each joined requester gets its own folder, so a Part 1 pairing in worker\\ is never overwritten."""
    from .paths import worker_dir

    if len(device_id) != 64 or any(c not in "0123456789abcdef" for c in device_id):
        raise ValueError("A full device ID is required.")
    return worker_dir(f"requesters/{device_id[:16]}")


def join_requester(host: str, port: int, expected_device_id: str, ident: Any, wdir: Path | None, cfg: WorkerConfig,
                   wait_s: float = 0, should_stop: Any = None) -> WorkerConfig:
    """Join an open group with this device's identity instead of a pairing code.

    The certificate seen here must hash to the device ID that was announced (or chosen), the helper signs the
    requester's fresh challenge bound to both device IDs, and the result is pinned exactly like a pairing.
    """
    import time

    from .identity import PeerBook, join_message

    host = _checked_address(host, port)
    pem, fingerprint = fetch_server_cert(host, port)
    if expected_device_id and not constant_time_equal(fingerprint, expected_device_id):
        raise ControllerError(0, "wrong_requester", "The computer at this address is not the requester you chose "
                              "(its certificate is different). Nothing was sent.")
    if PeerBook().is_blocked(fingerprint):
        raise ControllerError(0, "requester_blocked", "You blocked this requester. Unblock it first.")
    deadline = time.monotonic() + max(0.0, wait_s)
    with httpx.Client(base_url=controller_url(host, port), verify=_pem_context(pem), timeout=20) as http:
        while True:
            hello = _hello(http, fingerprint)
            signature = ident.sign(join_message(hello["challenge"], fingerprint, ident.device_id))
            resp = http.post("/join", json={
                "challenge": hello["challenge"], "name": ident.name, "device_cert": ident.cert_pem,
                "signature": base64.b64encode(signature).decode("ascii"),
                "info": hardware_info() | {"client_version": __version__},
                "policy_summary": {"join_mode": cfg.join_mode, "max_cpu_threads": cfg.max_cpu_threads,
                                   "max_memory_mb": cfg.max_memory_mb},
            })
            if resp.status_code != 202:
                body = raise_for_error(resp)
                break
            if time.monotonic() >= deadline or (should_stop and should_stop()):
                raise JoinPending(202, "pending", "The requester's owner has not accepted yet. Try again in a moment.")
            time.sleep(5)
    wdir = wdir or joined_dir(fingerprint)
    wdir.mkdir(parents=True, exist_ok=True)
    cert_path = wdir / "controller.crt"
    cert_path.write_text(pem, encoding="utf-8")
    protect_path(cert_path)
    cfg.controller_host, cfg.controller_port = host, port
    cfg.controller_fingerprint = fingerprint
    cfg.known_hosts = [host]
    cfg.name, cfg.worker_id, cfg.token = body["worker_name"], body["worker_id"], body["token"]
    cfg.group_mode = "open"
    cfg.requester_device_id = fingerprint
    cfg.requester_name = str(hello.get("requester_name", ""))[:40]
    save_config(cfg, wdir / "worker.json")
    PeerBook().remember(fingerprint, cfg.requester_name or "requester")
    return cfg


def _worker_http(cfg: WorkerConfig, wdir: Path) -> httpx.Client:
    return httpx.Client(base_url=controller_url(cfg.controller_host, cfg.controller_port),
                        verify=pinned_context(wdir / "controller.crt"),
                        headers={"Authorization": f"Bearer {cfg.token}"}, timeout=15)


def verify_requester(cfg: WorkerConfig, wdir: Path, ident: Any) -> str:
    """Verify (numeric comparison): returns the 6-digit number that the requester's screen must also show."""
    from .identity import check_commitment, new_nonce, verify_code

    with _worker_http(cfg, wdir) as http:
        start = raise_for_error(http.post("/worker/verify/start", json={}))
        requester_id = str(start.get("requester_device_id", ""))
        if not constant_time_equal(requester_id, cfg.controller_fingerprint):
            raise ControllerError(0, "verify_failed", "The requester answered with another identity. Do not trust it.")
        mine = new_nonce()
        theirs = str(raise_for_error(http.post("/worker/verify/finish", json={"session": start["session"], "nonce": mine}))["nonce"])
    try:
        valid = check_commitment(str(start["commitment"]), theirs, ident.device_id, requester_id)
    except ValueError:
        valid = False
    if not valid:
        raise ControllerError(0, "verify_failed", "The requester's answer does not match what it committed to. Do not trust it.")
    return verify_code(ident.device_id, requester_id, mine, theirs)


def requester_identity(admin: AdminClient) -> dict[str, Any]:
    return admin._call("GET", "/admin/identity")


def join_policy(admin: AdminClient, **changes: Any) -> dict[str, Any]:
    """Read the join policy, or change join_policy / announce / announce_label / redundancy_percent."""
    changes = {k: v for k, v in changes.items() if v is not None}
    return admin._call("PUT", "/admin/policy", json=changes) if changes else admin._call("GET", "/admin/policy")


def close_group(admin: AdminClient, closing: bool) -> dict[str, Any]:
    return admin._call("POST", "/admin/close", json={"closing": closing})


def requester_devices(admin: AdminClient) -> dict[str, Any]:
    return admin._call("GET", "/admin/devices")


def device_action(admin: AdminClient, ref: str, action: str) -> dict[str, Any]:
    return admin._call("POST", f"/admin/devices/{ref}/{action}")


def leave_requester(cfg: WorkerConfig, wdir: Path) -> None:
    """Tell the requester this helper leaves (its token is dropped there), then forget the token here."""
    try:
        with _worker_http(cfg, wdir) as http:
            raise_for_error(http.post("/worker/leave", json={}))
    finally:
        cfg.token = cfg.worker_id = ""
        save_config(cfg, wdir / "worker.json")
