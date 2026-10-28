"""Phase 6 spike, HELPER side (run on the i3, or any PC acting as a helper). Throwaway code.

  --mode listen : receive-only announcement listener on the current network (no Wi-Fi change)
  --mode join   : CONCURRENCY TEST. While connected to the normal Wi-Fi, look for DIRECT-NC-XXXX,
                  join it (passphrase derived from the SSID, PLAN §15.3), record whether the normal
                  Wi-Fi stays connected, listen for announcements on the group, ping the GO, then
                  remove the temporary profile and reconnect the original Wi-Fi profile.
  --method winrt|netsh : how to join (WinRT WiFiAdapter.connect_async needs no profile file; netsh is
                  the 0.2.0 path with a temporary ACL-protected profile XML).

The listener never sends. No TCP port is opened. No firewall rule is changed.
"""
from __future__ import annotations

import argparse
import asyncio
import ipaddress
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from xml.sax.saxutils import escape

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(1, str(HERE.parents[1]))
import mcast_spike  # noqa: E402
from nc_spike_common import OUT, SSID_RE, capture, connected_profile, connected_ssid, derive_passphrase  # noqa: E402

PROFILE_RE = re.compile(r"[A-Za-z0-9 _.()'-]{1,64}")


def listen(seconds: float) -> str:
    ifs = mcast_spike.interfaces()
    lines = [f"{time.strftime('%H:%M:%S')} listening {seconds:.0f}s on {ifs}"]
    stop = threading.Event()
    seen: list = []
    socks = []
    for alias, ip, _ in ifs:
        try:
            socks.append((f"{alias}:{ip}", mcast_spike.open_listener(ip, [ip], exclusive=True)))
        except OSError as exc:
            lines.append(f"  bind {ip}:{mcast_spike.PORT} failed: {exc}")
    threads = [threading.Thread(target=mcast_spike.listen_loop, args=(n, s, stop, seen, True), daemon=True) for n, s in socks]
    for t in threads:
        t.start()
    try:
        time.sleep(seconds)
    except KeyboardInterrupt:
        pass
    stop.set()
    for t in threads:
        t.join()
    for _, s in socks:
        s.close()
    counts: dict = {}
    for name, src, size, info in seen:
        dest = info.split("dest=")[1].split()[0] if "dest=" in info else "?"
        key = (name, src, dest)
        counts[key] = counts.get(key, 0) + 1
    lines.append("RECEIVED (listener, source, kind): count")
    lines += [f"  {k}: {v}" for k, v in sorted(counts.items())] or ["  nothing received"]
    if seen:
        ages = [float(i.split("age=")[1].rstrip("s")) for *_, i in seen if "age=" in i]
        if ages:
            lines.append(f"  age min/max {min(ages):.3f}/{max(ages):.3f} s (includes clock difference between PCs)")
    text = "\n".join(lines)
    print(text, flush=True)
    return text


async def winrt_find(ssid: str | None):
    from winrt.windows.devices.wifi import WiFiAdapter, WiFiAccessStatus

    access = await WiFiAdapter.request_access_async()
    print("WiFiAdapter access:", WiFiAccessStatus(access).name)
    adapters = await WiFiAdapter.find_all_adapters_async()
    for a in adapters:
        await a.scan_async()
        nets = [n for n in a.network_report.available_networks if SSID_RE.fullmatch(n.ssid)]
        print("DIRECT-NC networks seen:", [(n.ssid, n.network_rssi_in_decibel_milliwatts) for n in nets])
        for n in nets:
            if ssid is None or n.ssid == ssid:
                return a, n
    return None, None


async def winrt_connect(adapter, network) -> str:
    from winrt.windows.devices.wifi import WiFiReconnectionKind, WiFiConnectionStatus
    from winrt.windows.security.credentials import PasswordCredential

    cred = PasswordCredential()
    cred.password = derive_passphrase(network.ssid)
    t0 = time.perf_counter()
    result = await adapter.connect_with_password_credential_async(network, WiFiReconnectionKind.MANUAL, cred)
    return f"connect_async -> {WiFiConnectionStatus(result.connection_status).name} in {(time.perf_counter() - t0):.1f} s"


def netsh_connect(ssid: str) -> str:
    from network_connector.security import protect_path

    name = "NC-spike-" + ssid[-4:]
    xml = f'''<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{name}</name>
<SSIDConfig><SSID><name>{escape(ssid)}</name></SSID></SSIDConfig><connectionType>ESS</connectionType>
<connectionMode>manual</connectionMode><MSM><security><authEncryption><authentication>WPA2PSK</authentication>
<encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType>
<protected>false</protected><keyMaterial>{derive_passphrase(ssid)}</keyMaterial></sharedKey></security></MSM></WLANProfile>'''
    with tempfile.TemporaryDirectory(prefix="nc-spike-") as d:
        p = Path(d) / "profile.xml"
        p.touch()
        protect_path(p)
        p.write_text(xml, encoding="utf-8")
        add = subprocess.run(["netsh", "wlan", "add", "profile", f"filename={p}", "user=current"],
                             capture_output=True, text=True, errors="replace", timeout=30)
    con = subprocess.run(["netsh", "wlan", "connect", f"name={name}", f"ssid={ssid}"],
                         capture_output=True, text=True, errors="replace", timeout=30)
    return f"add profile exit {add.returncode}: {add.stdout.strip()}\nconnect exit {con.returncode}: {con.stdout.strip()}"


def ping(host: str) -> str:
    p = subprocess.run(["ping", "-n", "5", str(ipaddress.IPv4Address(host))], capture_output=True, text=True,
                       errors="replace", timeout=40)
    return p.stdout


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["listen", "join"], required=True)
    ap.add_argument("--ssid", help="DIRECT-NC-XXXX shown by the requester script (default: first one found)")
    ap.add_argument("--method", choices=["winrt", "netsh"], default="winrt")
    ap.add_argument("--seconds", type=float, default=90)
    args = ap.parse_args()
    OUT.mkdir(exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    report = capture(f"helper {args.mode} BEFORE", firewall=True)
    if args.mode == "listen":
        report += listen(args.seconds)
    else:
        if args.ssid and not SSID_RE.fullmatch(args.ssid):
            raise SystemExit("--ssid must look like DIRECT-NC-AB12")
        home_ssid, home_profile = connected_ssid(), connected_profile()
        report += f"\nNormal Wi-Fi before: ssid={home_ssid!r} profile={home_profile!r}\n"
        adapter, network = asyncio.run(winrt_find(args.ssid))
        if network is None:
            report += "No matching DIRECT-NC network visible. Is the requester script running in -Mode direct?\n"
        else:
            print("\nThis will try to join", network.ssid, "while connected to", repr(home_ssid))
            if input("Type START to continue: ") == "START":
                before_ips = {ip for _, ip, _ in mcast_spike.interfaces()}
                try:
                    if args.method == "winrt":
                        report += asyncio.run(winrt_connect(adapter, network)) + "\n"
                    else:
                        report += netsh_connect(network.ssid) + "\n"
                    time.sleep(15)
                    report += capture("helper JOINED")
                    report += (f"\nCONCURRENCY: normal Wi-Fi after join: ssid={connected_ssid()!r} "
                               f"(before {home_ssid!r})\n")
                    new = [(a, ip, pre) for a, ip, pre in mcast_spike.interfaces() if ip not in before_ips]
                    report += f"New IPv4 addresses after join: {new}\n"
                    report += listen(args.seconds)
                    for _, ip, pre in new:
                        net = ipaddress.IPv4Network(f"{ip}/{pre}", strict=False)
                        report += ping(str(net.network_address + 1))
                finally:
                    if args.method == "winrt":
                        adapter.disconnect()
                    subprocess.run(["netsh", "wlan", "delete", "profile", f"name=NC-spike-{network.ssid[-4:]}"],
                                   capture_output=True, timeout=30)
                    subprocess.run(["netsh", "wlan", "delete", "profile", f"name={network.ssid}"],
                                   capture_output=True, timeout=30)
                    if home_profile and PROFILE_RE.fullmatch(home_profile):
                        subprocess.run(["netsh", "wlan", "connect", f"name={home_profile}"], capture_output=True, timeout=30)
                    time.sleep(10)
                    report += capture("helper AFTER restore")
    path = OUT / f"helper-{args.mode}-{args.method}-{stamp}.txt"
    path.write_text(report, encoding="utf-8")
    print(f"Saved {path}")


if __name__ == "__main__":
    main()
