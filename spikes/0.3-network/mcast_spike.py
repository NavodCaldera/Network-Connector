"""Phase 6 spike: UDP multicast + broadcast announcements (PLAN §15.1-15.2). Throwaway code.

Run with the PROJECT venv (needs only stdlib + psutil; cryptography is optional for sizing):
  .venv\\Scripts\\python.exe spikes\\0.3-network\\mcast_spike.py selftest
  .venv\\Scripts\\python.exe spikes\\0.3-network\\mcast_spike.py listen --seconds 120          (helper, i3)
  .venv\\Scripts\\python.exe spikes\\0.3-network\\mcast_spike.py send --seconds 120            (requester, i7)

The listener is receive-only: it never calls sendto() on its sockets.
The sender binds each socket to one explicit interface address, never 0.0.0.0.
"""
from __future__ import annotations

import argparse
import base64
import ipaddress
import json
import os
import socket
import struct
import sys
import threading
import time

GROUP = "239.255.78.67"
PORT = 8766
MAX_PACKET = 1200
SO_EXCLUSIVEADDRUSE = getattr(socket, "SO_EXCLUSIVEADDRUSE", -5)


def interfaces(include_loopback: bool = False) -> list[tuple[str, str, int]]:
    """(alias, ipv4, prefix) for every up interface with a private/link-local IPv4."""
    import psutil

    stats = psutil.net_if_stats()
    out = []
    for alias, addrs in psutil.net_if_addrs().items():
        if alias in stats and not stats[alias].isup:
            continue
        for a in addrs:
            if a.family != socket.AF_INET or not a.netmask:
                continue
            ip = ipaddress.IPv4Address(a.address)
            if ip.is_loopback and not include_loopback:
                continue
            if not (ip.is_private or ip.is_link_local or ip.is_loopback):
                continue
            prefix = ipaddress.IPv4Network(f"0.0.0.0/{a.netmask}").prefixlen
            out.append((alias, str(ip), prefix))
    return out


def announcement(host: str, seq: int) -> bytes:
    body = {"v": 1, "device_id": "0" * 64, "name": socket.gethostname()[:32], "host": host, "port": 8765,
            "open": True, "need": {"threads": 8, "est_minutes": 5}, "job_label": "spike", "sandbox": "wasm",
            "ts": time.time(), "seq": seq}
    raw = json.dumps(body, separators=(",", ":"), sort_keys=True).encode()
    sig = b"\0" * 72  # placeholder size of a DER ECDSA P-256 signature
    try:
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec

        sig = ec.generate_private_key(ec.SECP256R1()).sign(raw, ec.ECDSA(hashes.SHA256()))
    except ImportError:
        pass
    packet = json.dumps({"a": base64.b64encode(raw).decode(), "s": base64.b64encode(sig).decode()},
                        separators=(",", ":")).encode()
    return packet


def open_listener(bind_ip: str, join_ifaces: list[str], exclusive: bool) -> socket.socket:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    if exclusive and sys.platform == "win32":
        s.setsockopt(socket.SOL_SOCKET, SO_EXCLUSIVEADDRUSE, 1)
    s.bind((bind_ip, PORT))
    for iface in join_ifaces:
        mreq = struct.pack("4s4s", socket.inet_aton(GROUP), socket.inet_aton(iface))
        try:
            s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
        except OSError as exc:
            print(f"  join {GROUP} on {iface} failed: {exc}")
    s.settimeout(0.5)
    return s


def listen_loop(name: str, s: socket.socket, stop: threading.Event, seen: list, verbose: bool) -> None:
    while not stop.is_set():
        try:
            data, src = s.recvfrom(65535)
        except socket.timeout:
            continue
        except OSError as exc:
            print(f"  [{name}] recv error: {exc}")
            continue
        ok = len(data) <= MAX_PACKET
        info = "?"
        try:
            outer = json.loads(data)
            inner = json.loads(base64.b64decode(outer["a"]))
            info = f"host={inner.get('host')} dest={inner.get('dest', '?')} seq={inner.get('seq')} age={time.time() - inner['ts']:.3f}s"
        except Exception as exc:
            info = f"unparsable ({type(exc).__name__})"
        seen.append((name, src[0], len(data), info))
        if verbose:
            print(f"  [{name}] from {src[0]}:{src[1]} {len(data)} B {'ok' if ok else 'TOO BIG'} {info}")


def tag(packet_host: str, dest: str, seq: int) -> bytes:
    raw_outer = json.loads(announcement(packet_host, seq))
    inner = json.loads(base64.b64decode(raw_outer["a"]))
    inner["dest"] = dest
    raw_outer["a"] = base64.b64encode(json.dumps(inner, separators=(",", ":")).encode()).decode()
    return json.dumps(raw_outer, separators=(",", ":")).encode()


def send_once(iface_ip: str, prefix: int, seq: int, broadcast: bool, loop: bool = True) -> list[str]:
    sent = []
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    try:
        s.bind((iface_ip, 0))
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(iface_ip))
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
        s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1 if loop else 0)
        try:
            s.sendto(tag(iface_ip, "multicast", seq), (GROUP, PORT))
            sent.append("multicast")
        except OSError as exc:
            sent.append(f"multicast FAILED {exc}")
        if broadcast:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
            net = ipaddress.IPv4Network(f"{iface_ip}/{prefix}", strict=False)
            for dest, label in ((str(net.broadcast_address), "subnet-bcast"), ("255.255.255.255", "limited-bcast")):
                try:
                    s.sendto(tag(iface_ip, label, seq), (dest, PORT))
                    sent.append(label)
                except OSError as exc:
                    sent.append(f"{label} FAILED {exc}")
    finally:
        s.close()
    return sent


def cmd_selftest(args: argparse.Namespace) -> None:
    ifs = interfaces(include_loopback=True)
    print("Interfaces:", ifs)
    pkt = announcement("192.168.137.1", 1)
    print(f"Signed announcement size: {len(pkt)} bytes (limit {MAX_PACKET}); "
          f"cryptography={'yes' if b'AAAA' not in pkt else 'placeholder'}")
    stop = threading.Event()
    seen: list = []
    socks = []
    lan = [ip for _, ip, _ in ifs if not ip.startswith("127.")]
    # Variant 1: one socket per interface, bound to that interface's address.
    for alias, ip, _ in ifs:
        try:
            s = open_listener(ip, [ip], exclusive=True)
            socks.append((f"bound:{ip}", s))
        except OSError as exc:
            print(f"  listener bind {ip}:{PORT} failed: {exc}")
    threads = [threading.Thread(target=listen_loop, args=(n, s, stop, seen, False), daemon=True) for n, s in socks]
    for t in threads:
        t.start()
    time.sleep(0.3)
    for alias, ip, prefix in ifs:
        print(f"send via {alias} {ip}: {send_once(ip, prefix, 1, broadcast=True)}")
    time.sleep(1.5)
    stop.set()
    for t in threads:
        t.join()
    print("\nReceived (listener, source, bytes, info) with per-interface bound sockets:")
    for row in sorted(seen):
        print("  ", row)
    for _, s in socks:
        s.close()

    # Exclusive-use check: a second socket on the same address/port must fail.
    print("\nExclusive address use:")
    a = open_listener(lan[0] if lan else "127.0.0.1", [], exclusive=True)
    for exclusive in (False, True):
        try:
            b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            if not exclusive:
                b.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            b.bind((lan[0] if lan else "127.0.0.1", PORT))
            print(f"  second bind (SO_REUSEADDR={not exclusive}) SUCCEEDED -> hijack possible")
            b.close()
        except OSError as exc:
            print(f"  second bind (SO_REUSEADDR={not exclusive}) refused: {exc}")
    a.close()
    # Without exclusive on the first socket, can SO_REUSEADDR steal the port?
    a = open_listener(lan[0] if lan else "127.0.0.1", [], exclusive=False)
    try:
        b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        b.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        b.bind((lan[0] if lan else "127.0.0.1", PORT))
        print("  first socket WITHOUT exclusive; second with SO_REUSEADDR SUCCEEDED -> hijack possible")
        b.close()
    except OSError as exc:
        print(f"  first socket without exclusive; second with SO_REUSEADDR refused: {exc}")
    a.close()
    # Wildcard first, then a more specific bind by another socket (traffic-stealing check).
    for exclusive in (False, True):
        w = open_listener("0.0.0.0", [], exclusive=exclusive)
        try:
            b = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            b.bind((lan[0] if lan else "127.0.0.1", PORT))
            print(f"  wildcard (exclusive={exclusive}) then specific bind SUCCEEDED -> more-specific socket can steal")
            b.close()
        except OSError as exc:
            print(f"  wildcard (exclusive={exclusive}) then specific bind refused: {exc}")
        w.close()

    # Variant 2: a wildcard-bound socket (for comparison only; the listener design question).
    print("\nWildcard-bound socket (comparison):")
    stop2 = threading.Event()
    seen2: list = []
    w = open_listener("0.0.0.0", [ip for _, ip, _ in ifs], exclusive=True)
    t = threading.Thread(target=listen_loop, args=("wildcard", w, stop2, seen2, False), daemon=True)
    t.start()
    time.sleep(0.3)
    for alias, ip, prefix in ifs:
        send_once(ip, prefix, 2, broadcast=True)
    time.sleep(1.5)
    stop2.set()
    t.join()
    w.close()
    for row in sorted(seen2):
        print("  ", row)


def cmd_listen(args: argparse.Namespace) -> None:
    ifs = interfaces(include_loopback=False)
    print(time.strftime("%Y-%m-%d %H:%M:%S"), socket.gethostname(), "interfaces:", ifs)
    stop = threading.Event()
    seen: list = []
    socks = []
    for alias, ip, _ in ifs:
        try:
            socks.append((f"{alias}:{ip}", open_listener(ip, [ip], exclusive=True)))
            print(f"  listening (receive-only) on {ip}:{PORT}, joined {GROUP}")
        except OSError as exc:
            print(f"  bind {ip}:{PORT} failed: {exc}")
    threads = [threading.Thread(target=listen_loop, args=(n, s, stop, seen, True), daemon=True) for n, s in socks]
    for t in threads:
        t.start()
    try:
        time.sleep(args.seconds)
    except KeyboardInterrupt:
        pass
    stop.set()
    for t in threads:
        t.join()
    print("\nSUMMARY (listener, source, dest-kind): count")
    counts: dict = {}
    for name, src, _, info in seen:
        dest = info.split("dest=")[1].split()[0] if "dest=" in info else "?"
        counts[(name, src, dest)] = counts.get((name, src, dest), 0) + 1
    for k, v in sorted(counts.items()):
        print("  ", k, v)
    if not counts:
        print("   nothing received")


def cmd_send(args: argparse.Namespace) -> None:
    ifs = interfaces(include_loopback=False)
    print(time.strftime("%Y-%m-%d %H:%M:%S"), socket.gethostname(), "sending on:", ifs)
    end = time.monotonic() + args.seconds
    seq = 0
    while time.monotonic() < end:
        seq += 1
        for alias, ip, prefix in ifs:
            res = send_once(ip, prefix, seq, broadcast=True, loop=False)
            if seq == 1 or any("FAILED" in r for r in res):
                print(f"  seq {seq} via {alias} {ip}: {res}")
        time.sleep(2)
    print(f"sent {seq} rounds")


def main() -> None:
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("selftest")
    lp = sub.add_parser("listen")
    lp.add_argument("--seconds", type=float, default=120)
    sp = sub.add_parser("send")
    sp.add_argument("--seconds", type=float, default=120)
    args = p.parse_args()
    {"selftest": cmd_selftest, "listen": cmd_listen, "send": cmd_send}[args.cmd](args)


if __name__ == "__main__":
    main()
