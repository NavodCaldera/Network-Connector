"""Throwaway: item 2 follow-up. Does UDP really leave the container? Can anything connect in?"""
from __future__ import annotations

import socket
import sys
import threading
import time

import ac
from t2_escape import show

LAN_IP = sys.argv[1] if len(sys.argv) > 1 else "10.63.182.8"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main() -> None:
    got: dict[str, list] = {"127.0.0.1": [], LAN_IP: []}
    socks = {}
    for host in got:
        u = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        u.bind((host, 0))
        socks[host] = u

        def rx(u=u, host=host) -> None:
            while True:
                data, peer = u.recvfrom(100)
                got[host].append((data, peer))
        threading.Thread(target=rx, daemon=True).start()
    udp_args = [x for h, u in socks.items() for x in ("--udp", f"{h}:{u.getsockname()[1]}")]

    tcp_p, udp_p = free_port(), free_port()
    inbound: list[str] = []

    def knock() -> None:
        end = time.monotonic() + 2.5
        while time.monotonic() < end:
            for host in ("127.0.0.1", LAN_IP):
                try:
                    with socket.create_connection((host, tcp_p), timeout=0.3):
                        inbound.append(f"tcp {host} CONNECTED")
                except OSError as exc:
                    inbound.append(f"tcp {host} {type(exc).__name__}")
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
                    s.sendto(b"to-sandbox", (host, udp_p))
            time.sleep(0.2)

    for label, kw in (("CONTROL plain process", dict(appcontainer=False, job=False, cwd=str(ac.HERE / "build"))),
                      ("SANDBOX AppContainer + Job", {})):
        for v in got.values():
            v.clear()
        inbound.clear()
        t = threading.Timer(0.6, knock)
        t.start()
        r = ac.run(ac.probe_cmd(*udp_args, "--accept", str(tcp_p), "--udp-recv", str(udp_p)), **kw)
        t.join()
        show(label, r)
        print("   parent UDP receivers got:", {h: len(v) for h, v in got.items()})
        print("   parent inbound attempts:", sorted(set(inbound)))
    print("leftovers:", ac.leftovers())


if __name__ == "__main__":
    main()
