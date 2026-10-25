"""Tokens, pairing codes, certificates and file protection. No secret is ever hard-coded."""
from __future__ import annotations

import datetime as dt
import hashlib
import hmac
import ipaddress
import os
import secrets
import subprocess
import sys
from pathlib import Path

# No 0/O/1/I/L so a code read aloud or typed from a screen is hard to get wrong.
_CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"


def new_token() -> str:
    return secrets.token_urlsafe(32)


def hash_token(token: str) -> str:
    """Tokens are 256-bit random values, so a plain SHA-256 is sufficient for storage."""
    return hashlib.sha256(token.encode()).hexdigest()


def new_pairing_code() -> str:
    """12 characters from a 31-letter alphabet is about 59 bits, shown as XXXX-XXXX-XXXX."""
    raw = "".join(secrets.choice(_CODE_ALPHABET) for _ in range(12))
    return f"{raw[:4]}-{raw[4:8]}-{raw[8:]}"


def normalize_code(code: str) -> str:
    return "".join(ch for ch in code.upper() if ch.isalnum())


def pairing_proof(code: str, role: str, fingerprint: str, nonce: str) -> str:
    """Proves knowledge of the pairing code AND binds it to the certificate the worker saw.

    If someone impersonates the controller with a different certificate, the proof the
    real controller expects will not match, and no token is ever issued.
    """
    key = normalize_code(code).encode()
    msg = f"{role}|{fingerprint}|{nonce}".encode()
    return hmac.new(key, msg, hashlib.sha256).hexdigest()


def constant_time_equal(a: str, b: str) -> bool:
    return hmac.compare_digest(a.encode(), b.encode())


def fingerprint_der(der: bytes) -> str:
    return hashlib.sha256(der).hexdigest()


def fingerprint_pem_file(path: Path) -> str:
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization

    cert = x509.load_pem_x509_certificate(path.read_bytes())
    return fingerprint_der(cert.public_bytes(serialization.Encoding.DER))


def protect_path(path: Path) -> None:
    """Restrict a secret file to the current user (best effort, no admin rights needed)."""
    try:
        if sys.platform == "win32":
            user = os.environ.get("USERNAME", "")
            if user:
                subprocess.run(
                    ["icacls", str(path), "/inheritance:r", "/grant:r", f"{user}:F"],
                    capture_output=True,
                    check=False,
                    timeout=15,
                )
        else:
            path.chmod(0o600)
    except Exception:  # noqa: BLE001 - never fail the app because ACL tightening failed
        pass


def generate_self_signed(cert_path: Path, key_path: Path, hosts: list[str]) -> None:
    """Create the controller's own TLS certificate (EC P-256, 2 years) and private key."""
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "network-connector-controller")])
    sans: list[x509.GeneralName] = [x509.DNSName("localhost")]
    for host in hosts:
        try:
            sans.append(x509.IPAddress(ipaddress.ip_address(host)))
        except ValueError:
            sans.append(x509.DNSName(host))
    now = dt.datetime.now(dt.timezone.utc)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - dt.timedelta(minutes=5))
        .not_valid_after(now + dt.timedelta(days=730))
        .add_extension(x509.SubjectAlternativeName(sans), critical=False)
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .sign(key, hashes.SHA256())
    )
    cert_path.write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    key_path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    protect_path(key_path)


def load_or_create_admin_token(path: Path) -> str:
    if path.is_file():
        return path.read_text(encoding="utf-8").strip()
    token = new_token()
    path.write_text(token, encoding="utf-8")
    protect_path(path)
    return token
