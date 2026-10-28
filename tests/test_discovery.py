"""Signed announcements and the receive-only listener (PLAN §15, §22.2)."""
import base64
import json
import socket
import time

import pytest

from conftest import wait_for
from network_connector import discovery, identity
from network_connector.discovery import (
    ANNOUNCE_DOMAIN,
    AnnouncementError,
    Announcer,
    Listener,
    NearbyEntry,
    NearbyList,
    build_announcement,
    parse_announcement,
)
from network_connector.security import generate_self_signed

LAN = "192.168.1.20"


@pytest.fixture(scope="module")
def idents(tmp_path_factory):
    base = tmp_path_factory.mktemp("ids")
    return identity.load_or_create(base / "a"), identity.load_or_create(base / "b")


def forge(ident, packet_body=None, sign_with=None, **changes):
    body = json.loads(json.loads(build_announcement(ident, LAN, 8765, open_=True))["p"])
    body.update(changes)
    if packet_body is not None:
        body = packet_body
    text = json.dumps(body, separators=(",", ":"))
    sig = (sign_with or ident).sign(ANNOUNCE_DOMAIN + text.encode())
    return json.dumps({"p": text, "s": base64.b64encode(sig).decode()}).encode()


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test_roundtrip_and_size(idents, tmp_path):
    a, _ = idents
    packet = build_announcement(a, LAN, 8765, open_=True, threads=12, est_minutes=3, job_label="primes", helpers=2)
    assert len(packet) <= discovery.MAX_PACKET
    ann = parse_announcement(packet, LAN)
    assert (ann.device_id, ann.name, ann.host, ann.port, ann.open, ann.threads, ann.helpers) == (a.device_id, a.name, LAN, 8765, True, 12, 2)
    # A migrated 0.2.0 controller certificate (with LAN SANs) still fits in one packet.
    generate_self_signed(tmp_path / "s.crt", tmp_path / "s.key", ["192.168.137.1", "10.63.182.8", "127.0.0.1"])
    old = identity.Identity.from_files(tmp_path / "s.crt", tmp_path / "s.key", "x" * 40)
    assert len(build_announcement(old, LAN, 8765, open_=True, job_label="y" * 60)) <= discovery.MAX_PACKET


@pytest.mark.parametrize("data", [
    b"", b"x" * 1201, b"\xff\xfe", b"not json", b"[]", b'{"p":"{}"}', b'{"p":"{}","s":"","x":1}',
    b'{"p":1,"s":"AA=="}', b'{"p":"{\\"v\\":NaN}","s":"AA=="}',
])
def test_malformed_packets_are_dropped(data):
    with pytest.raises(AnnouncementError):
        parse_announcement(data, LAN)


@pytest.mark.parametrize("changes", [
    {"port": "8765"}, {"port": 80}, {"open": "yes"}, {"v": 2}, {"sandbox": "native"}, {"helpers": -1},
    {"need": {"threads": 1}}, {"need": {"threads": True, "est_minutes": 0}}, {"name": ""}, {"name": "a\nb"},
    {"job_label": "z" * 61}, {"device_id": "AB" * 32}, {"cert": "!!!"}, {"ts": 1.5},
])
def test_bad_fields_are_dropped(idents, changes):
    a, _ = idents
    with pytest.raises(AnnouncementError):
        parse_announcement(forge(a, **changes), LAN)


def test_extra_or_missing_fields_are_dropped(idents):
    a, _ = idents
    body = json.loads(json.loads(build_announcement(a, LAN, 8765, open_=True))["p"])
    with pytest.raises(AnnouncementError):
        parse_announcement(forge(a, packet_body=body | {"cmd": "run"}), LAN)
    body.pop("job_label")
    with pytest.raises(AnnouncementError):
        parse_announcement(forge(a, packet_body=body), LAN)


def test_old_and_future_announcements_are_dropped(idents):
    a, _ = idents
    now = time.time()
    parse_announcement(build_announcement(a, LAN, 8765, open_=True, ts=int(now) - 9), LAN, now=now)
    for ts in (int(now) - 11, int(now) + 11):
        with pytest.raises(AnnouncementError, match="old"):
            parse_announcement(build_announcement(a, LAN, 8765, open_=True, ts=ts), LAN, now=now)


def test_wrong_signatures_are_dropped(idents):
    a, b = idents
    packet = json.loads(build_announcement(a, LAN, 8765, open_=True))
    packet["p"] = packet["p"].replace('"open":true', '"open":false')
    with pytest.raises(AnnouncementError, match="signature"):
        parse_announcement(json.dumps(packet).encode(), LAN)
    with pytest.raises(AnnouncementError, match="signature"):
        parse_announcement(forge(a, sign_with=b), LAN)  # a's certificate, signed by b


def test_trusted_device_id_with_another_key_is_refused(idents):
    trusted, attacker = idents
    # The attacker signs correctly with its own key but claims the trusted device's ID.
    with pytest.raises(AnnouncementError, match="does not match the certificate"):
        parse_announcement(forge(attacker, device_id=trusted.device_id), LAN)


def test_host_must_be_the_sender_and_private(idents):
    a, _ = idents
    with pytest.raises(AnnouncementError, match="sender"):
        parse_announcement(build_announcement(a, LAN, 8765, open_=True), "192.168.1.99")
    with pytest.raises(AnnouncementError, match="private"):
        parse_announcement(build_announcement(a, "8.8.8.8", 8765, open_=True), "8.8.8.8")
    with pytest.raises(AnnouncementError, match="private"):
        parse_announcement(build_announcement(a, "127.0.0.1", 8765, open_=True), "127.0.0.1")


def test_listener_drops_own_replayed_and_floods(idents):
    a, b = idents
    nearby = NearbyList()
    listener = Listener(b.device_id, nearby, interfaces=lambda: [])
    now = int(time.time())
    assert not listener.handle_packet(build_announcement(b, LAN, 8765, open_=True), LAN)  # our own
    assert listener.handle_packet(build_announcement(a, LAN, 8765, open_=True, ts=now), LAN)
    assert not listener.handle_packet(build_announcement(a, LAN, 8765, open_=True, ts=now - 2), LAN)  # older replay
    assert [e.device_id for e in nearby.entries()] == [a.device_id]
    flood = Listener(b.device_id, NearbyList(), interfaces=lambda: [])
    accepted = sum(flood.handle_packet(build_announcement(a, LAN, 8765, open_=True), LAN) for _ in range(50))
    assert accepted <= 10 and flood.limited >= 40
    garbage = Listener(b.device_id, NearbyList(), interfaces=lambda: [])
    for i in range(300):
        garbage.handle_packet(b"junk", f"10.0.{i // 250}.{i % 250 + 1}")
    assert garbage.limited >= 100  # the overall limit holds across many sources


def test_real_udp_announcement_reaches_receive_only_listener(idents):
    a, b = idents
    port = free_udp_port()
    nearby = NearbyList()
    listener = Listener(b.device_id, nearby, port=port, include_loopback=True,
                        interfaces=lambda: [("Loopback", "127.0.0.1", 8)])
    listener.start()
    try:
        assert listener.bound == ["127.0.0.1"]
        sock = listener._sockets["127.0.0.1"][0]
        assert sock.getsockname() == ("127.0.0.1", port)  # an explicit address, never a wildcard
        assert sock.type == socket.SOCK_DGRAM
        announcer = Announcer(a, lambda: ["127.0.0.1"], lambda: {"port": 8765, "open": True, "helpers": 1},
                              dest_port=port, extra_targets=[("127.0.0.1", port)])
        assert announcer.send_once() == 1
        entry = wait_for(lambda: nearby.get(a.device_id), timeout=5, interval=0.05)
        assert entry.host == "127.0.0.1" and entry.link == "loopback" and entry.helpers == 1
        second = Listener(b.device_id, NearbyList(), port=port, include_loopback=True,
                          interfaces=lambda: [("Loopback", "127.0.0.1", 8)])
        second._rescan()  # the exclusive bind refuses a second socket: the port is reported busy
        assert "127.0.0.1" in second.busy and not second.bound
    finally:
        listener.stop()


def test_listener_never_binds_wildcard_or_unwanted_loopback(idents):
    _, b = idents
    listener = Listener(b.device_id, NearbyList(), port=free_udp_port(),
                        interfaces=lambda: [("any", "0.0.0.0", 0), ("lo", "127.0.0.1", 8)])
    listener._rescan()
    assert listener.bound == []


def test_announcer_refuses_wildcard_and_public(idents):
    a, _ = idents
    announcer = Announcer(a, lambda: ["0.0.0.0", "8.8.8.8"], lambda: {"port": 8765, "open": True},
                          extra_targets=[("127.0.0.1", 9)])
    assert announcer.send_once() == 0


def test_nearby_list_expires_and_bounds():
    nearby = NearbyList(ttl_s=0.05)
    seen = []
    nearby.on_new = seen.append
    nearby.update(NearbyEntry("aa" * 32, "x", LAN, 8765))
    nearby.update(NearbyEntry("aa" * 32, "x", LAN, 8765))
    assert len(seen) == 1 and nearby.entries()
    time.sleep(0.1)
    assert nearby.entries() == []
    for i in range(discovery.MAX_ENTRIES + 10):
        nearby.ttl_s = 60
        nearby.update(NearbyEntry(f"{i:064x}", "x", LAN, 8765))
    assert len(nearby.entries()) == discovery.MAX_ENTRIES


def test_probe_fallback_uses_outbound_hello(monkeypatch):
    from network_connector import client

    calls = []

    def hello(host, port, timeout=3):
        calls.append(host)
        return {"device_id": "cd" * 32, "name": "i7", "open": True, "policy": "anyone"} if host == "192.168.137.1" else None

    monkeypatch.setattr(client, "requester_hello", hello)
    found = discovery.probe_requesters(["192.168.137.1", "192.168.137.1", "192.168.137.2"], 8765)
    assert calls == ["192.168.137.1", "192.168.137.2"]
    assert [(e.device_id, e.link, e.source) for e in found] == [("cd" * 32, "wifi-direct", "probe")]
    assert discovery.probe_requesters(["192.168.137.1"], 8765, own_device_id="cd" * 32) == []
