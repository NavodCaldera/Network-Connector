"""Phase 6 spike, REQUESTER side (run on the i7). Throwaway code.

  -Mode router   : announce on the current LAN for --seconds (no Wi-Fi change)
  -Mode direct   : start a legacy Wi-Fi Direct group DIRECT-NC-XXXX (passphrase derived from the SSID,
                   PLAN §15.3), capture the GO adapter, announce on every interface, then stop the group
  -Mode hotspot  : you start Windows Mobile Hotspot by hand; the script captures it and announces

Announcements are sent from sockets bound to each interface address (never 0.0.0.0). Nothing listens.
"""
from __future__ import annotations

import argparse
import secrets
import string
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mcast_spike  # noqa: E402
from nc_spike_common import OUT, capture, derive_passphrase  # noqa: E402


def announce(seconds: float, log: list[str]) -> None:
    end = time.monotonic() + seconds
    seq = 0
    while time.monotonic() < end:
        seq += 1
        for alias, ip, prefix in mcast_spike.interfaces():
            res = mcast_spike.send_once(ip, prefix, seq, broadcast=True, loop=False)
            if seq == 1 or seq % 15 == 0 or any("FAILED" in r for r in res):
                line = f"{time.strftime('%H:%M:%S')} seq {seq} via {alias} {ip}/{prefix}: {res}"
                print(line, flush=True)
                log.append(line)
        time.sleep(2)


def start_group(ssid: str):
    from winrt.windows.devices.wifidirect import WiFiDirectAdvertisementPublisher, WiFiDirectAdvertisementPublisherStatus
    from winrt.windows.security.credentials import PasswordCredential

    pub = WiFiDirectAdvertisementPublisher()
    pub.advertisement.is_autonomous_group_owner_enabled = True
    legacy = pub.advertisement.legacy_settings
    legacy.is_enabled = True
    legacy.ssid = ssid
    cred = PasswordCredential()
    cred.password = derive_passphrase(ssid)
    legacy.passphrase = cred
    pub.start()
    for _ in range(50):
        if pub.status != WiFiDirectAdvertisementPublisherStatus.CREATED:
            break
        time.sleep(0.2)
    print(f"publisher status: {WiFiDirectAdvertisementPublisherStatus(pub.status).name}", flush=True)
    return pub


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["router", "direct", "hotspot"], required=True)
    ap.add_argument("--seconds", type=float, default=240)
    args = ap.parse_args()
    OUT.mkdir(exist_ok=True)
    report = capture(f"requester {args.mode} BEFORE", firewall=True)
    log: list[str] = []
    pub = None
    try:
        if args.mode == "direct":
            ssid = "DIRECT-NC-" + "".join(secrets.choice(string.ascii_uppercase + string.digits) for _ in range(4))
            print(f"\n>>> SSID: {ssid}   (on the helper run: run-helper.ps1 -Mode join -Ssid {ssid})\n", flush=True)
            pub = start_group(ssid)
            time.sleep(5)
            report += capture("requester direct GROUP STARTED")
        elif args.mode == "hotspot":
            input("Start Mobile Hotspot in Settings > Network & internet (note its SSID/password), then press Enter: ")
            report += capture("requester hotspot STARTED")
        t = threading.Thread(target=announce, args=(args.seconds, log), daemon=True)
        t.start()
        print(f"Announcing for {args.seconds:.0f} s. Start the helper script now. Ctrl+C to stop early.", flush=True)
        try:
            while t.is_alive():
                t.join(1)
        except KeyboardInterrupt:
            pass
        report += capture(f"requester {args.mode} DURING (after announcing)")
    finally:
        if pub is not None:
            pub.stop()
            time.sleep(3)
            report += capture("requester direct GROUP STOPPED")
    path = OUT / f"requester-{args.mode}-{time.strftime('%Y%m%d-%H%M%S')}.txt"
    path.write_text(report + "\n".join(log), encoding="utf-8")
    print(f"Saved {path}")


if __name__ == "__main__":
    main()
