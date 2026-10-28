"""Phase 6 spike: read Wi-Fi state and find DIRECT-NC-* networks without netsh text parsing. Throwaway code.

Run with the SPIKE venv:
  spikes\\0.3-network\\.venv\\Scripts\\python.exe spikes\\0.3-network\\wifi_state_spike.py

Read-only: lists connection profiles, asks for Wi-Fi adapter access, triggers one scan and
prints the visible SSIDs. It never connects, disconnects or changes a profile.
"""
from __future__ import annotations

import asyncio
import re
import subprocess
import time

DIRECT_NC = re.compile(r"^DIRECT-NC-[A-Za-z0-9-]{1,22}$")


async def main() -> None:
    from winrt.windows.networking.connectivity import NetworkInformation, NetworkConnectivityLevel
    from winrt.windows.devices.wifi import WiFiAdapter, WiFiAccessStatus

    print(time.strftime("%Y-%m-%d %H:%M:%S"))
    print("Connection profiles:")
    for p in NetworkInformation.get_connection_profiles():
        level = NetworkConnectivityLevel(p.get_network_connectivity_level()).name
        wlan = p.is_wlan_connection_profile
        ssid = p.wlan_connection_profile_details.get_connected_ssid() if wlan else ""
        cat = ""
        try:
            names = p.get_network_names()
            cat = ",".join(names)
        except Exception as exc:
            cat = f"names error {type(exc).__name__}"
        if level != "NONE" or wlan:
            ad = p.network_adapter
            print(f"  name={p.profile_name!r} wlan={wlan} ssid={ssid!r} connectivity={level} network_names={cat!r} "
                  f"iftype={ad.iana_interface_type if ad else '?'}")

    access = await WiFiAdapter.request_access_async()
    print("WiFiAdapter.request_access_async ->", WiFiAccessStatus(access).name)
    adapters = await WiFiAdapter.find_all_adapters_async()
    print("Wi-Fi adapters:", len(adapters))
    for a in adapters:
        t0 = time.perf_counter()
        try:
            await a.scan_async()
            print(f"  scan_async ok ({(time.perf_counter() - t0) * 1000:.0f} ms)")
        except Exception as exc:
            print(f"  scan_async failed: {type(exc).__name__}: {exc}")
        nets = list(a.network_report.available_networks)
        ssids = sorted({n.ssid for n in nets})
        print(f"  visible networks: {len(nets)} BSSIDs, {len(ssids)} SSIDs")
        for n in sorted(nets, key=lambda n: -n.network_rssi_in_decibel_milliwatts)[:15]:
            print(f"    ssid={n.ssid!r} rssi={n.network_rssi_in_decibel_milliwatts} dBm "
                  f"freq={n.channel_center_frequency_in_kilohertz // 1000} MHz kind={n.network_kind} "
                  f"direct_nc={bool(DIRECT_NC.match(n.ssid))}")
        print("  DIRECT-* SSIDs:", [s for s in ssids if s.startswith("DIRECT-")])

    proc = subprocess.run(["netsh", "wlan", "show", "networks"], capture_output=True, text=True, errors="replace", timeout=20)
    print(f"\nnetsh wlan show networks (exit {proc.returncode}), first lines:")
    for line in proc.stdout.splitlines()[:8]:
        print("   ", line)


if __name__ == "__main__":
    asyncio.run(main())
