"""Device identity: one EC P-256 key and self-signed certificate per install (PLAN §14).

- Device ID = SHA-256 of the certificate (the same value as the Part 1 certificate fingerprint).
- Short ID = the first 8 characters, shown as AB12-CD34.
- The files live in %LOCALAPPDATA%\\NetworkConnector\\identity and are ACL-protected.
- An existing controller certificate (controller\\server.crt + server.key) is adopted as the device identity,
  so workers paired with 0.2.0 keep working without pairing again. Nothing is ever regenerated over it.

Also here: signatures for joining and announcements, the helper's own trusted/blocked list, and the
commit-then-reveal numeric comparison used by Verify.
"""
from __future__ import annotations

import datetime as dt
import hashlib
import hmac
import ipaddress
import json
import re
import secrets
import shutil
import socket
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .paths import data_home
from .security import fingerprint_der, protect_path

CERT_NAME, KEY_NAME, INFO_NAME, PEERS_NAME = "device.crt", "device.key", "identity.json", "peers.json"
DEVICE_ID_RE = re.compile(r"^[0-9a-f]{64}$")
SHORT_ID_RE = re.compile(r"^[0-9A-Fa-f]{4}-?[0-9A-Fa-f]{4}$")
MAX_NAME = 40
JOIN_DOMAIN = b"nc-join-v1"
_lock = threading.Lock()
_cache: dict[Path, "Identity"] = {}


class IdentityError(RuntimeError):
    """The identity files are missing, mismatched or unreadable."""


def short_id(device_id: str) -> str:
    return f"{device_id[:4]}-{device_id[4:8]}".upper()


def clean_name(name: str) -> str:
    """A friendly name: printable, single line, at most 40 characters."""
    text = "".join(ch for ch in " ".join(str(name).split()) if ch.isprintable())[:MAX_NAME].strip()
    if not text:
        raise ValueError("The device name must contain at least one visible character.")
    return text


def default_name() -> str:
    try:
        return clean_name(socket.gethostname())
    except ValueError:
        return "Network Connector device"


# ------------------------------------------------------------------ certificates and signatures
def load_cert(data: str | bytes) -> Any:
    """Parse a PEM (text) or DER (bytes) certificate."""
    from cryptography import x509

    if isinstance(data, str):
        return x509.load_pem_x509_certificate(data.encode("ascii"))
    return x509.load_der_x509_certificate(data)


def cert_der(cert: Any) -> bytes:
    from cryptography.hazmat.primitives import serialization

    return cert.public_bytes(serialization.Encoding.DER)


def device_id_of(cert: Any) -> str:
    return fingerprint_der(cert_der(cert))


def verify_signature(cert: Any, data: bytes, signature: bytes) -> bool:
    """True only for a valid ECDSA P-256 / SHA-256 signature by this certificate's key."""
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec

    key = cert.public_key()
    if not isinstance(key, ec.EllipticCurvePublicKey) or not isinstance(key.curve, ec.SECP256R1):
        return False
    try:
        key.verify(signature, data, ec.ECDSA(hashes.SHA256()))
        return True
    except (InvalidSignature, ValueError):
        return False


EXPIRY_WARN_DAYS = 90


def cert_expires(path: Path) -> dt.datetime:
    cert = load_cert(Path(path).read_text(encoding="ascii"))
    after = getattr(cert, "not_valid_after_utc", None)
    return after if after is not None else cert.not_valid_after.replace(tzinfo=dt.timezone.utc)


def expiry_warning(path: Path, now: dt.datetime | None = None, warn_days: int = EXPIRY_WARN_DAYS) -> str:
    """A plain-English warning when a pinned certificate expires soon (TLS refuses it afterwards), else "".

    Certificates are never renewed silently: the device ID is the certificate's hash, so a new certificate is
    a new identity and every helper must pair or join again. A 0.2.0 controller certificate lasts 2 years.
    """
    try:
        expires = cert_expires(path)
    except (OSError, ValueError):
        return ""
    left = expires - (now or dt.datetime.now(dt.timezone.utc))
    if left.total_seconds() <= 0:
        return f"The certificate expired on {expires:%Y-%m-%d}. Connections are refused until you renew it (docs/SECURITY.md)."
    if left.days < warn_days:
        return (f"The certificate expires on {expires:%Y-%m-%d} ({left.days} days). Plan to renew it: "
                "helpers must pair or join again afterwards (docs/SECURITY.md).")
    return ""


def join_message(challenge: str, requester_id: str, helper_id: str) -> bytes:
    """What a helper signs to join. Binding the requester ID stops a relay to another requester."""
    return b"\n".join([JOIN_DOMAIN, challenge.encode("ascii"), requester_id.encode("ascii"), helper_id.encode("ascii")])


def _generate(cert_path: Path, key_path: Path) -> None:
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "network-connector-device")])
    now = dt.datetime.now(dt.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - dt.timedelta(minutes=5))
        # The device ID is the hash of this certificate, so it must outlive the install.
        .not_valid_after(now + dt.timedelta(days=365 * 20))
        .add_extension(x509.SubjectAlternativeName(
            [x509.DNSName("localhost"), x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]), critical=False)
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .sign(key, hashes.SHA256())
    )
    # Write and protect the key before the certificate, so a half-written identity is never adopted.
    key_path.write_bytes(b"")
    protect_path(key_path)
    key_path.write_bytes(key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    protect_path(cert_path)


@dataclass
class Identity:
    device_id: str
    short_id: str
    name: str
    cert_pem: str
    key_path: Path
    cert_path: Path
    _key: Any = field(default=None, repr=False, compare=False)

    @classmethod
    def from_files(cls, cert_path: Path, key_path: Path, name: str | None = None) -> "Identity":
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ec

        try:
            pem = cert_path.read_text(encoding="ascii")
            cert = load_cert(pem)
            key = serialization.load_pem_private_key(key_path.read_bytes(), password=None)
        except (OSError, ValueError) as exc:
            raise IdentityError(f"The device identity in {cert_path.parent} cannot be read: {exc}") from exc
        if not isinstance(key, ec.EllipticCurvePrivateKey) or key.public_key().public_numbers() != cert.public_key().public_numbers():
            raise IdentityError("The device certificate and private key do not match. Restore the matching pair from backup.")
        device_id = device_id_of(cert)
        return cls(device_id, short_id(device_id), name or default_name(), pem, key_path, cert_path, key)

    def sign(self, data: bytes) -> bytes:
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec

        return self._key.sign(data, ec.ECDSA(hashes.SHA256()))

    @property
    def cert_der(self) -> bytes:
        return cert_der(load_cert(self.cert_pem))


# ------------------------------------------------------------------ the device identity on disk
def identity_dir(home: Path | None = None) -> Path:
    path = (home or data_home()) / "identity"
    if not path.is_dir():
        path.mkdir(parents=True, exist_ok=True)
        protect_path(path)
    return path


def _read_info(directory: Path) -> dict[str, Any]:
    try:
        data = json.loads((directory / INFO_NAME).read_text(encoding="utf-8"))
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def _write_info(directory: Path, info: dict[str, Any]) -> None:
    path = directory / INFO_NAME
    path.write_text(json.dumps(info, indent=2), encoding="utf-8")
    protect_path(path)


def load_or_create(home: Path | None = None) -> Identity:
    """Load this install's identity, adopting a 0.2.0 controller certificate or creating a new key once."""
    home = home or data_home()
    directory = identity_dir(home)
    cert, key = directory / CERT_NAME, directory / KEY_NAME
    info = _read_info(directory)
    if cert.is_file() != key.is_file():
        raise IdentityError(f"Half of the device identity is missing in {directory}. Restore it from backup; "
                            "refusing to replace it, because every helper would have to join again.")
    if not cert.is_file():
        # Never call controller_dir() here: it would create a controller folder on helper-only PCs.
        old_cert, old_key = home / "controller" / "server.crt", home / "controller" / "server.key"
        if old_cert.is_file() and old_key.is_file():
            Identity.from_files(old_cert, old_key)  # refuse to adopt a mismatched pair
            shutil.copyfile(old_key, key)
            protect_path(key)
            shutil.copyfile(old_cert, cert)
            protect_path(cert)
            info["migrated_from"] = "controller/server.crt"
        else:
            _generate(cert, key)
        info.setdefault("created", dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"))
    name = info.get("name")
    try:
        name = clean_name(name) if name else default_name()
    except ValueError:
        name = default_name()
    if info.get("name") != name:
        info["name"] = name
    _write_info(directory, info)
    return Identity.from_files(cert, key, name)


def current(home: Path | None = None) -> Identity:
    """The device identity, loaded once per data folder (AGENT.md §4 shared contract)."""
    directory = (home or data_home()).resolve()
    with _lock:
        ident = _cache.get(directory)
        if ident is None:
            ident = _cache[directory] = load_or_create(directory)
        return ident


def set_name(name: str, home: Path | None = None) -> Identity:
    directory = identity_dir(home)
    info = _read_info(directory)
    info["name"] = clean_name(name)
    _write_info(directory, info)
    with _lock:
        _cache.pop((home or data_home()).resolve(), None)
    return current(home)


def install_server_cert(cert_path: Path, key_path: Path, ident: Identity) -> None:
    """Use the device identity as the requester's TLS certificate (a new controller only)."""
    if cert_path.exists() or key_path.exists():
        raise IdentityError("The controller already has a certificate; refusing to replace it.")
    cert_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ident.key_path, key_path)
    protect_path(key_path)
    shutil.copyfile(ident.cert_path, cert_path)


# ------------------------------------------------------------------ this device's trusted and blocked lists
def normalize_device_ref(ref: str) -> str:
    """A full device ID (64 hex) or a short ID prefix (AB12-CD34), lower-case without the dash."""
    text = ref.strip().lower()
    if DEVICE_ID_RE.match(text):
        return text
    if SHORT_ID_RE.match(text):
        return text.replace("-", "")
    raise ValueError("Use a full device ID (64 hex characters) or a short ID such as AB12-CD34.")


class PeerBook:
    """Trusted and blocked devices as this computer sees them (used by the helper side)."""

    def __init__(self, path: Path | None = None) -> None:
        self.path = path or identity_dir() / PEERS_NAME
        self._lock = threading.Lock()

    def _load(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            data = {}
        if not isinstance(data, dict):
            data = {}
        return {"trusted": [d for d in data.get("trusted", []) if isinstance(d, str) and DEVICE_ID_RE.match(d)],
                "blocked": [d for d in data.get("blocked", []) if isinstance(d, str) and DEVICE_ID_RE.match(d)],
                "names": {k: str(v)[:MAX_NAME] for k, v in (data.get("names") or {}).items() if DEVICE_ID_RE.match(str(k))}}

    def _save(self, data: dict[str, Any]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(json.dumps(data, indent=2), encoding="utf-8")
        protect_path(self.path)

    def resolve(self, ref: str) -> str:
        """Expand a short ID to a full ID this computer already knows; full IDs pass through."""
        ref = normalize_device_ref(ref)
        if len(ref) == 64:
            return ref
        data = self._load()
        known = {*data["trusted"], *data["blocked"], *data["names"]}
        matches = sorted(d for d in known if d.startswith(ref))
        if len(matches) != 1:
            raise ValueError("Unknown or ambiguous short ID. Use the full device ID.")
        return matches[0]

    def is_trusted(self, device_id: str) -> bool:
        return device_id in self._load()["trusted"]

    def is_blocked(self, device_id: str) -> bool:
        return device_id in self._load()["blocked"]

    def blocked(self) -> list[str]:
        return list(self._load()["blocked"])

    def set(self, device_id: str, *, trusted: bool | None = None, blocked: bool | None = None, name: str | None = None) -> None:
        if not DEVICE_ID_RE.match(device_id):
            raise ValueError("A full device ID is required.")
        with self._lock:
            data = self._load()
            for key, value in (("trusted", trusted), ("blocked", blocked)):
                if value is True and device_id not in data[key]:
                    data[key].append(device_id)
                elif value is False and device_id in data[key]:
                    data[key].remove(device_id)
            if blocked:  # a blocked device is never also trusted
                data["trusted"] = [d for d in data["trusted"] if d != device_id]
            if name:
                data["names"][device_id] = clean_name(name)
            self._save(data)

    def remember(self, device_id: str, name: str) -> None:
        with self._lock:
            data = self._load()
            if data["names"].get(device_id) != name:
                try:
                    data["names"][device_id] = clean_name(name)
                except ValueError:
                    return
                self._save(data)

    def snapshot(self) -> dict[str, Any]:
        return self._load()


# ------------------------------------------------------------------ Verify: numeric comparison (commit, then reveal)
def new_nonce() -> str:
    return secrets.token_hex(16)


def commitment(nonce_hex: str, helper_id: str, requester_id: str) -> str:
    """The requester commits to its nonce before it sees the helper's one (as in BLE numeric comparison)."""
    msg = b"nc-verify-commit-v1" + bytes.fromhex(helper_id) + bytes.fromhex(requester_id)
    return hmac.new(bytes.fromhex(nonce_hex), msg, hashlib.sha256).hexdigest()


def check_commitment(commit_hex: str, nonce_hex: str, helper_id: str, requester_id: str) -> bool:
    return hmac.compare_digest(commitment(nonce_hex, helper_id, requester_id), commit_hex)


def verify_code(helper_id: str, requester_id: str, helper_nonce: str, requester_nonce: str) -> str:
    """The 6-digit number both screens show. Each side's key and nonce changes it unpredictably."""
    digest = hashlib.sha256(b"nc-verify-code-v1" + bytes.fromhex(helper_id) + bytes.fromhex(requester_id)
                            + bytes.fromhex(helper_nonce) + bytes.fromhex(requester_nonce)).digest()
    return f"{int.from_bytes(digest[:8], 'big') % 1_000_000:06d}"
