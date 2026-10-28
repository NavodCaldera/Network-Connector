"""Requester-side verifier hook (PLAN §18): a small Wasm module, run on the requester's own machine in the same
sandbox as any other Wasm job, that accepts or rejects each result.

Verifier ABI (WASI preview 1 command, like every job module):
  stdin  = 8-byte little-endian length of the job input, the job input, then the helper's output
  exit 0 = accept the result; any other exit, a trap or a limit = reject it
A rejected result goes back to the queue for a different helper, and the helper's reliability score drops.
"""
from __future__ import annotations

import base64
import binascii
import logging
import struct
import threading
from typing import Any, Callable

from . import sandbox
from .store import Store

log = logging.getLogger("network_connector.verify")

VERIFIER_MEMORY_MB = 256
VERIFIER_SECONDS = 60


def frame(job_input: bytes, output: bytes) -> bytes:
    return struct.pack("<Q", len(job_input)) + job_input + output


def unframe(data: bytes) -> tuple[bytes, bytes]:
    """The inverse of frame(); used by tests and by Python-side reference verifiers."""
    if len(data) < 8:
        raise ValueError("verifier input is too short")
    (size,) = struct.unpack("<Q", data[:8])
    if size > len(data) - 8:
        raise ValueError("verifier input length is wrong")
    return data[8:8 + size], data[8 + size:]


class VerifierRunner:
    """Takes 'verifying' jobs one at a time and records the verdict. One thread, started with the controller."""

    def __init__(self, store: Store, run: Callable[..., Any] | None = None) -> None:
        self.store = store
        self._run = run or sandbox.run
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        self._stop.clear()
        self._thread = threading.Thread(target=self._loop, name="result-verifier", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread and self._thread is not threading.current_thread():
            self._thread.join(timeout=5)

    def _loop(self) -> None:
        while not self._stop.wait(0.5):
            try:
                while not self._stop.is_set() and self.run_once():
                    pass
            except Exception:  # noqa: BLE001 - keep verifying later jobs
                log.exception("verifier loop error")

    def run_once(self) -> bool:
        job = self.store.next_verifying()
        if job is None:
            return False
        module = self.store.module_data(job["verifier"] or "")
        if module is None:
            self.store.verified(job["id"], False, "the verifier module is missing", usable=False)
            return True
        try:
            job_input = base64.b64decode(job["params"]["input_b64"], validate=True)
            output = base64.b64decode((job["result"] or {}).get("stdout_b64", ""), validate=True)
        except (binascii.Error, ValueError, KeyError, TypeError, AttributeError):
            self.store.verified(job["id"], False, "the result is not a valid wasm result")
            return True
        try:
            result = self._run(module, frame(job_input, output), [], memory_mb=VERIFIER_MEMORY_MB, seconds=VERIFIER_SECONDS)
        except sandbox.SandboxUnavailable as exc:
            self.store.verified(job["id"], False, str(exc), usable=False)
            return True
        accepted = result.ok and result.exit_code == 0
        detail = "" if accepted else f"verifier {result.outcome}, exit code {result.exit_code}"
        status = self.store.verified(job["id"], accepted, detail)
        log.info("job %s %s by the verifier -> %s", job["id"], "accepted" if accepted else "rejected", status)
        return True
