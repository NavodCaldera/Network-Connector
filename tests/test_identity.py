"""Device identity (PLAN §14): key, cert, IDs, migration of the 0.2.0 controller cert, and Verify."""
import hashlib
import re

import pytest

from network_connector import identity
from network_connector.config import ControllerConfig
from network_connector.controller import ControllerRuntime
from network_connector.security import fingerprint_pem_file, generate_self_signed


@pytest.fixture
def home(tmp_path, monkeypatch):
    monkeypatch.setenv("NC_HOME", str(tmp_path))
    identity._cache.clear()
    yield tmp_path
    identity._cache.clear()


def test_new_identity_ids_and_stability(home):
    ident = identity.current()
    cert, key = home / "identity" / "device.crt", home / "identity" / "device.key"
    assert cert.is_file() and key.is_file()
    assert ident.device_id == fingerprint_pem_file(cert) == hashlib.sha256(ident.cert_der).hexdigest()
    assert re.fullmatch(r"[0-9A-F]{4}-[0-9A-F]{4}", ident.short_id)
    assert ident.short_id.replace("-", "").lower() == ident.device_id[:8]
    assert ident.name and ident.key_path == key
    identity._cache.clear()
    assert identity.current().device_id == ident.device_id  # never regenerated


def test_identity_files_are_protected(home, monkeypatch):
    protected = []
    monkeypatch.setattr(identity, "protect_path", lambda p: protected.append(p.name))
    identity.load_or_create(home)
    assert {"device.key", "device.crt", "identity.json"} <= set(protected)


def test_sign_and_verify(home):
    ident = identity.current()
    sig = ident.sign(b"hello")
    cert = identity.load_cert(ident.cert_pem)
    assert identity.verify_signature(cert, b"hello", sig)
    assert not identity.verify_signature(cert, b"hellO", sig)
    other = identity.load_or_create(home / "other")
    assert not identity.verify_signature(identity.load_cert(other.cert_pem), b"hello", sig)


def test_half_missing_identity_is_refused(home):
    identity.current()
    (home / "identity" / "device.key").unlink()
    identity._cache.clear()
    with pytest.raises(identity.IdentityError):
        identity.current()


def test_migrates_existing_controller_certificate(home):
    controller = home / "controller"
    controller.mkdir()
    generate_self_signed(controller / "server.crt", controller / "server.key", ["192.168.1.4", "127.0.0.1"])
    before = fingerprint_pem_file(controller / "server.crt")
    ident = identity.current()
    assert ident.device_id == before  # Part 1 workers keep their pinned certificate
    assert identity._read_info(home / "identity")["migrated_from"] == "controller/server.crt"
    rt = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1"))
    try:
        assert rt.fingerprint == before == rt.identity.device_id
    finally:
        rt.store.close()


def test_new_controller_uses_device_identity(home):
    ident = identity.current()
    rt = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1"))
    try:
        assert rt.fingerprint == ident.device_id
        assert rt.identity.name == ident.name
    finally:
        rt.store.close()
    with pytest.raises(identity.IdentityError):
        identity.install_server_cert(rt.cert, rt.key, ident)  # never replaces a certificate


def test_names_and_refs(home):
    assert identity.set_name("  Navod's\ni7  ").name == "Navod's i7"
    assert identity.current().name == "Navod's i7"
    with pytest.raises(ValueError):
        identity.clean_name("\x00\x01")
    assert len(identity.clean_name("x" * 100)) == 40
    full = "ab" * 32
    assert identity.normalize_device_ref(full.upper()) == full
    assert identity.normalize_device_ref("ABAB-ABAB") == "abababab"
    with pytest.raises(ValueError):
        identity.normalize_device_ref("not-an-id")


def test_peer_book(home):
    book = identity.PeerBook()
    a, b = "a1" * 32, "b2" * 32
    book.set(a, trusted=True, name="i7")
    book.set(b, blocked=True)
    assert book.is_trusted(a) and book.is_blocked(b) and not book.is_trusted(b)
    assert book.resolve(identity.short_id(a)) == a
    book.set(a, blocked=True)
    assert not book.is_trusted(a) and book.is_blocked(a)  # blocking removes trust
    assert identity.PeerBook().is_blocked(b)  # persisted


def test_verify_numbers_match_and_commitment_binds_nonce():
    helper, requester = "11" * 32, "22" * 32
    nb = identity.new_nonce()
    commit = identity.commitment(nb, helper, requester)
    na = identity.new_nonce()
    assert identity.check_commitment(commit, nb, helper, requester)
    assert not identity.check_commitment(commit, identity.new_nonce(), helper, requester)
    assert not identity.check_commitment(commit, nb, "33" * 32, requester)  # another key changes the commitment
    code = identity.verify_code(helper, requester, na, nb)
    assert re.fullmatch(r"\d{6}", code)
    assert code == identity.verify_code(helper, requester, na, nb)
    assert len({identity.verify_code(helper, requester, identity.new_nonce(), nb) for _ in range(20)}) > 15


def test_expiry_warning_for_a_migrated_two_year_certificate(tmp_path):
    import datetime as dt

    from network_connector import identity
    from network_connector.security import generate_self_signed

    cert, key = tmp_path / "server.crt", tmp_path / "server.key"
    generate_self_signed(cert, key, ["127.0.0.1"])
    expires = identity.cert_expires(cert)
    assert identity.expiry_warning(cert, now=expires - dt.timedelta(days=200)) == ""
    soon = identity.expiry_warning(cert, now=expires - dt.timedelta(days=30))
    assert "expires on" in soon and "30 days" in soon and "pair or join again" in soon
    assert "expired" in identity.expiry_warning(cert, now=expires + dt.timedelta(days=1))
    assert identity.expiry_warning(tmp_path / "missing.crt") == ""
