"""HTTPS clients that trust ONLY the controller's own certificate (certificate pinning)."""
from __future__ import annotations

import secrets
import ssl
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
            base_url=f"https://{host}:{port}", verify=pinned_context(cert),
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


def admin_client_from_disk(controller_dir_: Path) -> AdminClient:
    from .config import ControllerConfig, load_config

    cfg = load_config(ControllerConfig, controller_dir_ / "controller.json")
    if not cfg.bind_host:
        raise ControllerError(0, "not_started", "The controller has not been started yet.")
    token = (controller_dir_ / "admin.token").read_text(encoding="utf-8").strip()
    return AdminClient(cfg.bind_host, cfg.port, controller_dir_ / "server.crt", token)


# ---------------------------------------------------------------- pairing (worker side)
def fetch_server_cert(host: str, port: int) -> tuple[str, str]:
    """Return (PEM, fingerprint) of the certificate the server presents right now."""
    pem = ssl.get_server_certificate((host, port), timeout=10)  # type: ignore[call-arg]
    der = ssl.PEM_cert_to_DER_cert(pem)
    return pem, fingerprint_der(der)


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
    with httpx.Client(base_url=f"https://{host}:{port}", verify=pinned_context(cert_path), timeout=20) as http:
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
    cfg.name, cfg.worker_id, cfg.token = name, body["worker_id"], body["token"]
    save_config(cfg, worker_dir / "worker.json")
    protect_path(cert_path)
    return cfg
