"""Update the worker from GitHub release tags (vX.Y.Z) and verify the result.

Only tagged releases are installed, so "approved version" means "a tag you published".
Configuration, tokens and certificates live in the data folder, outside the Git checkout,
so an update cannot replace them. If the post-update self-test fails, the previous
version is restored automatically.
"""
from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from . import __version__
from .paths import repo_root

TAG_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")


class UpdateError(RuntimeError):
    pass


def _ver(text: str) -> tuple[int, int, int]:
    m = TAG_RE.match(text if text.startswith("v") else "v" + text)
    if not m:
        raise UpdateError(f"not a release version: {text}")
    return int(m[1]), int(m[2]), int(m[3])


def _run(args: list[str], cwd: Path, timeout: int = 300) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as exc:
        raise UpdateError(f"Update command timed out after {timeout}s.") from exc
    except OSError as exc:
        raise UpdateError(f"Could not run update command: {exc}") from exc


def _git(repo: Path, *args: str) -> str:
    fetching = bool(args and args[0] == "fetch")
    try:
        res = _run(["git", *args], repo, timeout=30 if fetching else 300)
    except UpdateError as exc:
        if fetching:
            raise UpdateError("No internet connection; connect to a normal network to update. If already online, check Git and repository access.") from exc
        raise
    if res.returncode != 0:
        if fetching:
            raise UpdateError("No internet connection; connect to a normal network to update. If already online, check Git and repository access.")
        raise UpdateError(f"git {' '.join(args)} failed: {(res.stderr or res.stdout).strip()}")
    return res.stdout.strip()


def latest_tag(repo: Path) -> str | None:
    _git(repo, "fetch", "--tags", "--quiet", "origin")
    tags = [t for t in _git(repo, "tag", "-l", "v*").splitlines() if TAG_RE.match(t)]
    return max(tags, key=_ver) if tags else None


@dataclass
class UpdateStatus:
    current: str
    latest: str | None
    update_available: bool


def check() -> UpdateStatus:
    repo = repo_root()
    if repo is None:
        raise UpdateError("This installation is not a Git checkout, so it cannot self-update. Re-run setup-worker.ps1.")
    latest = latest_tag(repo)
    available = bool(latest and _ver(latest) > _ver(__version__))
    return UpdateStatus(__version__, latest, available)


def update(log: Callable[[str], None] = print, verify_signature: bool = False) -> str:
    """Install the newest release tag. Returns the new version. Raises UpdateError on failure (after rollback)."""
    repo = repo_root()
    if repo is None:
        raise UpdateError("This installation is not a Git checkout, so it cannot self-update.")
    if _git(repo, "status", "--porcelain"):
        raise UpdateError("The install folder has local changes; refusing to update over them.")
    status = check()
    if not status.update_available or status.latest is None:
        log(f"Already up to date (version {__version__}).")
        return __version__
    target = status.latest
    previous = _git(repo, "rev-parse", "HEAD")
    log(f"Updating {__version__} -> {target[1:]} ...")
    if verify_signature:
        res = _run(["git", "verify-tag", target], repo)
        if res.returncode != 0:
            raise UpdateError(f"Tag {target} has no valid signature: {res.stderr.strip()}")
    try:
        _git(repo, "checkout", "--quiet", "--detach", target)
        res = _run([sys.executable, "-m", "pip", "install", "--quiet", "-e", str(repo)], repo, timeout=900)
        if res.returncode != 0:
            raise UpdateError(f"pip install failed: {res.stderr.strip()[-400:]}")
        log("Verifying the new version ...")
        res = _run([sys.executable, "-m", "network_connector", "selftest"], repo, timeout=120)
        if res.returncode != 0 or f"version {target[1:]}" not in res.stdout:
            raise UpdateError(f"self-test of the new version failed: {(res.stdout + res.stderr).strip()[-400:]}")
    except Exception as exc:
        log(f"Update failed ({exc}). Restoring the previous version ...")
        _git(repo, "checkout", "--quiet", "--detach", previous)
        _run([sys.executable, "-m", "pip", "install", "--quiet", "-e", str(repo)], repo, timeout=900)
        raise UpdateError(f"{exc} (previous version restored)") from exc
    log(f"Updated to {target[1:]} and verified. Restart the worker to use it.")
    return target[1:]
