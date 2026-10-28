"""Nearby requesters: signed UDP announcements and a receive-only listener (PLAN §15, Phase 6 report §1).

Requester side (Announcer):
- one UDP socket per explicit listener address (never 0.0.0.0), multicast 239.255.78.67:8766 with TTL 1,
  plus the subnet-directed broadcast;
- every announcement is signed by the device key and says only where to connect and what is needed.

Helper side (Listener):
- one receive-only socket per interface address, bound exclusively to (ip, 8766). Never a wildcard bind;
- it never sends, never replies and never runs anything. Packets that are too big, malformed, old, replayed,
  wrongly signed, from our own device or not from the announced address are dropped, after a rate limit;
- a valid announcement can only add or refresh a line in the nearby list.

When announcements can't arrive (Public direct links, firewall), `probe_requesters()` asks the direct-link
gateway over outbound TLS instead. That needs no inbound rule.
"""
from __future__ import annotations

import base64
import binascii
import ipaddress
import json
import logging
import re
import select
import socket
import threading
import time
from collections import OrderedDict, deque
from dataclasses import asdict, dataclass, field
from typing import Any, Callable, Iterable

import psutil

from .identity import Identity, clean_name, device_id_of, load_cert, short_id, verify_signature

log = logging.getLogger("network_connector.discovery")

GROUP = "239.255.78.67"
PORT = 8766
MAX_PACKET = 1200
MAX_AGE_S = 10
ANNOUNCE_EVERY_S = 2.0
ENTRY_TTL_S = 12.0
QUIET_HINT_S = 30.0
ANNOUNCE_DOMAIN = b"nc-announce-v1\n"
BODY_KEYS = frozenset({"v", "device_id", "name", "host", "port", "open", "need", "job_label", "sandbox", "ts", "helpers", "cert"})
_VIRTUAL = re.compile(r"(?i)vethernet|wsl|hyper-v|virtualbox|vmware|loopback pseudo|docker|tailscale|zerotier|vpn")
_DIRECT_NET = ipaddress.IPv4Network("192.168.137.0/24")
MAX_ENTRIES = 64


class AnnouncementError(ValueError):
    """A packet was not a valid, fresh, signed announcement."""


class SlidingWindow:
    """At most `limit` events per `window_s` for each key, plus a bounded number of keys."""

    def __init__(self, limit: int, window_s: float, max_keys: int = 1024) -> None:
        self.limit, self.window_s, self.max_keys = limit, window_s, max_keys
        self._hits: OrderedDict[str, deque[float]] = OrderedDict()
        self._lock = threading.Lock()

    def allow(self, key: str, now: float | None = None) -> bool:
        now = time.monotonic() if now is None else now
        with self._lock:
            hits = self._hits.pop(key, None) or deque()
            while hits and now - hits[0] > self.window_s:
                hits.popleft()
            self._hits[key] = hits
            while len(self._hits) > self.max_keys:
                self._hits.popitem(last=False)
            if len(hits) >= self.limit:
                return False
            hits.append(now)
            return True


# ------------------------------------------------------------------ the announcement format
@dataclass
class Announcement:
    device_id: str
    name: str
    host: str
    port: int
    open: bool
    threads: int
    est_minutes: int
    job_label: str
    helpers: int
    ts: int


def _b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def build_announcement(ident: Identity, host: str, port: int, *, open_: bool, threads: int = 0, est_minutes: int = 0,
                       job_label: str = "", helpers: int = 0, ts: int | None = None) -> bytes:
    body = {
        "v": 1, "device_id": ident.device_id, "name": ident.name, "host": host, "port": int(port), "open": bool(open_),
        "need": {"threads": max(0, min(int(threads), 1024)), "est_minutes": max(0, min(int(est_minutes), 10080))},
        "job_label": job_label[:60], "sandbox": "wasm", "ts": int(time.time() if ts is None else ts),
        "helpers": max(0, min(int(helpers), 10000)), "cert": _b64(ident.cert_der),
    }
    text = json.dumps(body, separators=(",", ":"), ensure_ascii=True)
    packet = json.dumps({"p": text, "s": _b64(ident.sign(ANNOUNCE_DOMAIN + text.encode("ascii")))},
                        separators=(",", ":")).encode("ascii")
    if len(packet) > MAX_PACKET:
        raise AnnouncementError(f"announcement is {len(packet)} bytes; the limit is {MAX_PACKET}")
    return packet


def _no_constants(value: str) -> Any:
    raise AnnouncementError(f"non-finite number {value}")


def _int(value: Any, lo: int, hi: int, what: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not lo <= value <= hi:
        raise AnnouncementError(f"{what} must be an integer between {lo} and {hi}")
    return value


def parse_announcement(data: bytes, source_ip: str, now: float | None = None, allow_loopback: bool = False) -> Announcement:
    """Strictly check one packet. Cheap checks come first; the signature check is last."""
    if not isinstance(data, (bytes, bytearray)) or not 0 < len(data) <= MAX_PACKET:
        raise AnnouncementError("empty or oversized packet")
    try:
        outer = json.loads(bytes(data).decode("ascii"), parse_constant=_no_constants)
    except (UnicodeDecodeError, ValueError) as exc:
        raise AnnouncementError("not an ASCII JSON packet") from exc
    if not isinstance(outer, dict) or set(outer) != {"p", "s"} or not all(isinstance(outer[k], str) for k in ("p", "s")):
        raise AnnouncementError("wrong packet layout")
    text, sig_text = outer["p"], outer["s"]
    if len(sig_text) > 128:
        raise AnnouncementError("signature too long")
    try:
        body = json.loads(text, parse_constant=_no_constants)
    except ValueError as exc:
        raise AnnouncementError("body is not JSON") from exc
    if not isinstance(body, dict) or set(body) != BODY_KEYS:
        raise AnnouncementError("wrong announcement fields")
    if body["v"] != 1 or body["sandbox"] != "wasm" or not isinstance(body["open"], bool):
        raise AnnouncementError("unsupported announcement version")
    for key in ("device_id", "name", "host", "job_label", "cert"):
        if not isinstance(body[key], str):
            raise AnnouncementError(f"{key} must be text")
    if not re.fullmatch(r"[0-9a-f]{64}", body["device_id"]):
        raise AnnouncementError("bad device_id")
    ts = _int(body["ts"], 0, 2**40, "ts")
    now = time.time() if now is None else now
    if abs(now - ts) > MAX_AGE_S:
        raise AnnouncementError("announcement too old (or the clocks differ by more than 10 s)")
    port = _int(body["port"], 1024, 65535, "port")
    helpers = _int(body["helpers"], 0, 10000, "helpers")
    need = body["need"]
    if not isinstance(need, dict) or set(need) != {"threads", "est_minutes"}:
        raise AnnouncementError("bad need")
    threads = _int(need["threads"], 0, 1024, "need.threads")
    minutes = _int(need["est_minutes"], 0, 10080, "need.est_minutes")
    if len(body["job_label"]) > 60 or not all(ch.isprintable() for ch in body["job_label"]):
        raise AnnouncementError("bad job_label")
    try:
        name = clean_name(body["name"])
    except ValueError as exc:
        raise AnnouncementError("bad name") from exc
    if name != body["name"]:
        raise AnnouncementError("bad name")
    try:
        host = ipaddress.IPv4Address(body["host"])
        source = ipaddress.IPv4Address(source_ip)
    except ValueError as exc:
        raise AnnouncementError("host must be an IPv4 address") from exc
    if host != source:
        raise AnnouncementError("announced host is not the sender's address")
    if not (host.is_private or host.is_link_local or (allow_loopback and host.is_loopback)) or host.is_unspecified \
            or host.is_multicast or (host.is_loopback and not allow_loopback):
        raise AnnouncementError("host is not a private LAN address")
    try:
        der = base64.b64decode(body["cert"], validate=True)
        signature = base64.b64decode(sig_text, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise AnnouncementError("bad base64") from exc
    if len(der) > 1200 or not 8 <= len(signature) <= 80:
        raise AnnouncementError("certificate or signature has the wrong size")
    try:
        cert = load_cert(der)
    except ValueError as exc:
        raise AnnouncementError("bad certificate") from exc
    # The device ID is the hash of the certificate, so a trusted device's ID can't be claimed with another key.
    if device_id_of(cert) != body["device_id"]:
        raise AnnouncementError("device_id does not match the certificate")
    if not verify_signature(cert, ANNOUNCE_DOMAIN + text.encode("ascii"), signature):
        raise AnnouncementError("bad signature")
    return Announcement(body["device_id"], name, str(host), port, body["open"], threads, minutes,
                        body["job_label"], helpers, ts)


# ------------------------------------------------------------------ the nearby list
@dataclass
class NearbyEntry:
    device_id: str
    name: str
    host: str
    port: int
    open: bool = True
    threads: int = 0
    est_minutes: int = 0
    job_label: str = ""
    helpers: int = 0
    link: str = "lan"
    source: str = "announce"  # or "probe"
    last_seen: float = field(default_factory=time.time)

    @property
    def short_id(self) -> str:
        return short_id(self.device_id)

    def as_dict(self) -> dict[str, Any]:
        return asdict(self) | {"short_id": self.short_id}


class NearbyList:
    """Thread-safe list of requesters seen recently, newest information wins."""

    def __init__(self, ttl_s: float = ENTRY_TTL_S, on_new: Callable[[NearbyEntry], None] | None = None) -> None:
        self.ttl_s = ttl_s
        self.on_new = on_new
        self._items: dict[str, NearbyEntry] = {}
        self._lock = threading.Lock()

    def update(self, entry: NearbyEntry) -> None:
        with self._lock:
            known = entry.device_id in self._items
            if not known and len(self._items) >= MAX_ENTRIES:
                self._expire_locked(time.time())
                if len(self._items) >= MAX_ENTRIES:
                    return
            self._items[entry.device_id] = entry
        if not known and self.on_new:
            try:
                self.on_new(entry)
            except Exception:  # noqa: BLE001 - a UI callback must never stop the listener
                log.exception("nearby callback failed")

    def _expire_locked(self, now: float) -> None:
        for key in [k for k, e in self._items.items() if now - e.last_seen > (self.ttl_s * 3 if e.source == "probe" else self.ttl_s)]:
            del self._items[key]

    def entries(self) -> list[NearbyEntry]:
        with self._lock:
            self._expire_locked(time.time())
            return sorted(self._items.values(), key=lambda e: (not e.open, e.name.lower(), e.device_id))

    def get(self, device_id: str) -> NearbyEntry | None:
        with self._lock:
            return self._items.get(device_id)


# ------------------------------------------------------------------ interfaces
def interface_addresses(include_loopback: bool = False) -> list[tuple[str, str, int]]:
    """(interface name, IPv4, prefix) for every up, private interface, skipping virtual adapters."""
    out: list[tuple[str, str, int]] = []
    try:
        stats = psutil.net_if_stats()
        addrs = psutil.net_if_addrs()
    except (OSError, psutil.Error):
        return out
    for name, items in addrs.items():
        if name in stats and not stats[name].isup:
            continue
        for addr in items:
            if addr.family != socket.AF_INET:
                continue
            try:
                ip = ipaddress.IPv4Address(addr.address)
                prefix = ipaddress.IPv4Network(f"0.0.0.0/{addr.netmask}").prefixlen if addr.netmask else 24
            except ValueError:
                continue
            if ip.is_loopback:
                if include_loopback:
                    out.append((name, str(ip), 8))
                continue
            if ip.is_unspecified or _VIRTUAL.search(name) or not (ip.is_private or ip.is_link_local):
                continue
            out.append((name, str(ip), prefix))
    return out


def link_label(interface: str, ip: str) -> str:
    address = ipaddress.IPv4Address(ip)
    if address.is_loopback:
        return "loopback"
    if address in _DIRECT_NET or "direct" in interface.lower():
        return "wifi-direct"
    return "lan"


# ------------------------------------------------------------------ helper: receive-only listener
class Listener:
    """Receive-only discovery sockets. It has no send path at all."""

    def __init__(self, own_device_id: str, nearby: NearbyList, port: int = PORT, include_loopback: bool = False,
                 interfaces: Callable[[], list[tuple[str, str, int]]] | None = None, rescan_s: float = 10.0) -> None:
        self.own_device_id, self.nearby, self.port = own_device_id, nearby, port
        self.include_loopback = include_loopback
        self._interfaces = interfaces or (lambda: interface_addresses(include_loopback))
        self.rescan_s = rescan_s
        self._sockets: dict[str, tuple[socket.socket, str]] = {}
        self.busy: dict[str, str] = {}
        self.per_source = SlidingWindow(limit=10, window_s=5.0)
        self.overall = SlidingWindow(limit=200, window_s=1.0, max_keys=1)
        self._last_ts: dict[str, int] = {}
        self.accepted = self.dropped = self.limited = 0
        self.started = 0.0
        self.last_accepted = 0.0
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    # ---- lifecycle
    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop.clear()
        self.started = time.time()
        self._rescan()
        self._thread = threading.Thread(target=self._run, name="discovery-listener", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread and self._thread is not threading.current_thread():
            self._thread.join(timeout=3)
        for sock, _ in self._sockets.values():
            sock.close()
        self._sockets.clear()

    @property
    def bound(self) -> list[str]:
        return sorted(self._sockets)

    def status(self) -> dict[str, Any]:
        now = time.time()
        quiet = bool(self.started) and now - max(self.started, self.last_accepted) > QUIET_HINT_S
        if not self._sockets:
            hint = "Discovery is off: no network interface could be used. Use Find controller."
        elif self.busy and not self.accepted:
            hint = "Discovery port busy. Use Find controller."
        elif quiet:
            hint = "No announcements for 30 s. Discovery blocked? Use Find controller."
        else:
            hint = f"Listening on {len(self._sockets)} interface(s)."
        return {"bound": self.bound, "busy": dict(self.busy), "accepted": self.accepted, "dropped": self.dropped,
                "rate_limited": self.limited, "hint": hint}

    # ---- sockets
    def _open(self, ip: str) -> socket.socket:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
                # Nobody else can bind this address and port, and we never share it (Phase 6 report §1).
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
            sock.bind((ip, self.port))
            if not ipaddress.IPv4Address(ip).is_loopback:
                try:
                    sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, socket.inet_aton(GROUP) + socket.inet_aton(ip))
                except OSError as exc:
                    log.info("multicast join failed on %s (%s); broadcast still works", ip, exc)
            sock.setblocking(False)
            return sock
        except OSError:
            sock.close()
            raise

    def _rescan(self) -> None:
        wanted = {}
        for name, ip, _prefix in self._interfaces():
            address = ipaddress.IPv4Address(ip)
            if address.is_unspecified or (address.is_loopback and not self.include_loopback):
                continue  # never a wildcard bind
            wanted[ip] = name
        for ip in [i for i in self._sockets if i not in wanted]:
            self._sockets.pop(ip)[0].close()
        for ip, name in wanted.items():
            if ip in self._sockets:
                continue
            try:
                self._sockets[ip] = self._open(ip), name
                self.busy.pop(ip, None)
            except OSError as exc:
                self.busy[ip] = f"port {self.port} unavailable ({getattr(exc, 'winerror', None) or exc.errno})"

    def _run(self) -> None:
        next_scan = time.monotonic() + self.rescan_s
        while not self._stop.is_set():
            socks = [s for s, _ in self._sockets.values()]
            if not socks:
                self._stop.wait(1.0)
            else:
                try:
                    readable, _, _ = select.select(socks, [], [], 0.5)
                except (OSError, ValueError):
                    readable = []
                for sock in readable:
                    local = next(((ip, name) for ip, (s, name) in self._sockets.items() if s is sock), None)
                    for _ in range(32):
                        try:
                            data, addr = sock.recvfrom(MAX_PACKET + 1)
                        except (BlockingIOError, InterruptedError):
                            break
                        except OSError:
                            break  # e.g. WSAECONNRESET or a closed socket
                        if local:
                            self.handle_packet(data, addr[0], local[0], local[1])
            if time.monotonic() >= next_scan:
                next_scan = time.monotonic() + self.rescan_s
                try:
                    self._rescan()
                except Exception:  # noqa: BLE001 - keep listening on what we have
                    log.exception("discovery interface rescan failed")

    # ---- packet handling (also used directly by tests)
    def handle_packet(self, data: bytes, source_ip: str, local_ip: str = "", interface: str = "") -> bool:
        if not self.overall.allow("all") or not self.per_source.allow(source_ip):
            self.limited += 1
            return False
        try:
            ann = parse_announcement(data, source_ip, allow_loopback=self.include_loopback)
        except AnnouncementError as exc:
            self.dropped += 1
            log.debug("dropped announcement from %s: %s", source_ip, exc)
            return False
        if ann.device_id == self.own_device_id:
            return False  # Windows delivers our own multicast even with IP_MULTICAST_LOOP off
        if ann.ts < self._last_ts.get(ann.device_id, 0):
            self.dropped += 1
            return False
        if len(self._last_ts) > 4096:
            self._last_ts.clear()
        self._last_ts[ann.device_id] = ann.ts
        self.accepted += 1
        self.last_accepted = time.time()
        self.nearby.update(NearbyEntry(
            ann.device_id, ann.name, ann.host, ann.port, ann.open, ann.threads, ann.est_minutes, ann.job_label,
            ann.helpers, link_label(interface, local_ip or ann.host), "announce"))
        return True


# ------------------------------------------------------------------ requester: announcer
def _broadcast_address(ip: str) -> str | None:
    for _name, addr, prefix in interface_addresses():
        if addr == ip and prefix < 31:
            return str(ipaddress.IPv4Network(f"{ip}/{prefix}", strict=False).broadcast_address)
    return None


class Announcer:
    """Sends a signed announcement every 2 s from each explicit listener address."""

    def __init__(self, ident: Identity, hosts: Callable[[], Iterable[str]], state: Callable[[], dict[str, Any]],
                 dest_port: int = PORT, every_s: float = ANNOUNCE_EVERY_S,
                 extra_targets: list[tuple[str, int]] | None = None) -> None:
        self.ident, self._hosts, self._state = ident, hosts, state
        self.dest_port, self.every_s = dest_port, every_s
        self.extra_targets = extra_targets or []  # tests send unicast to a loopback listener
        self.sent = 0
        self.errors: dict[str, str] = {}
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name="discovery-announcer", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread and self._thread is not threading.current_thread():
            self._thread.join(timeout=3)

    @property
    def running(self) -> bool:
        return bool(self._thread and self._thread.is_alive())

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                self.send_once()
            except Exception:  # noqa: BLE001 - try again next round
                log.exception("announcement round failed")
            self._stop.wait(self.every_s)

    def send_once(self) -> int:
        from .config import validate_bind_host

        state = self._state()
        sent = 0
        for host in list(dict.fromkeys(self._hosts())):
            try:
                host = validate_bind_host(host)  # refuses 0.0.0.0, :: and public addresses
                address = ipaddress.IPv4Address(host)
            except ValueError:
                continue
            loopback = address.is_loopback
            targets = [] if loopback else [(GROUP, self.dest_port)]
            broadcast = None if loopback else _broadcast_address(host)
            if broadcast:
                targets.append((broadcast, self.dest_port))
            targets += [t for t in self.extra_targets if ipaddress.IPv4Address(t[0]).is_loopback == loopback]
            if not targets:
                continue
            packet = build_announcement(self.ident, host, state["port"], open_=state["open"], threads=state.get("threads", 0),
                                        est_minutes=state.get("est_minutes", 0), job_label=state.get("job_label", ""),
                                        helpers=state.get("helpers", 0))
            try:
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                    sock.bind((host, 0))  # explicit address only
                    if not loopback:
                        sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(host))
                        sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
                        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
                    for target in targets:
                        try:
                            sock.sendto(packet, target)
                            sent += 1
                        except OSError as exc:
                            self.errors[f"{host}->{target[0]}"] = str(exc)
                self.errors.pop(host, None)
            except OSError as exc:
                self.errors[host] = str(exc)
        self.sent += sent
        return sent


# ------------------------------------------------------------------ outbound fallback
def probe_requesters(hosts: Iterable[str] | None = None, port: int = 8765, timeout: float = 3.0,
                     own_device_id: str = "") -> list[NearbyEntry]:
    """Ask a bounded list of addresses (by default the direct-link gateway) who they are, over outbound TLS."""
    from .client import requester_hello
    from .wifidirect import DirectLinkError, candidate_controllers

    if hosts is None:
        try:
            hosts = candidate_controllers()
        except (DirectLinkError, OSError, RuntimeError):
            hosts = []
    found = []
    for host in list(dict.fromkeys(hosts))[:8]:
        info = requester_hello(host, port, timeout=timeout)
        if not info or info["device_id"] == own_device_id:
            continue
        found.append(NearbyEntry(info["device_id"], info["name"], host, port, info["open"],
                                 link="wifi-direct" if ipaddress.IPv4Address(host) in _DIRECT_NET else link_label("", host),
                                 source="probe"))
    return found
