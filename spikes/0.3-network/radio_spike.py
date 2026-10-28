"""Phase 6 spike: ask Windows to turn the Wi-Fi radio on (Windows.Devices.Radios). Throwaway code.

Run with the SPIKE venv:
  spikes\\0.3-network\\.venv\\Scripts\\python.exe spikes\\0.3-network\\radio_spike.py            (query only)
  spikes\\0.3-network\\.venv\\Scripts\\python.exe spikes\\0.3-network\\radio_spike.py --turn-on  (Wi-Fi only)

This script never turns a radio OFF and never touches Bluetooth or mobile broadband radios.
"""
from __future__ import annotations

import argparse
import asyncio
import ctypes
import time


async def main(turn_on: bool) -> None:
    from winrt.windows.devices.radios import Radio, RadioKind, RadioState, RadioAccessStatus

    print(time.strftime("%Y-%m-%d %H:%M:%S"), "admin:", bool(ctypes.windll.shell32.IsUserAnAdmin()))
    access = await Radio.request_access_async()
    print("Radio.request_access_async ->", RadioAccessStatus(access).name)
    radios = await Radio.get_radios_async()
    wifi = []
    for r in radios:
        print(f"  radio name={r.name!r} kind={RadioKind(r.kind).name} state={RadioState(r.state).name}")
        if r.kind == RadioKind.WI_FI:
            wifi.append(r)
    if not wifi:
        print("No Wi-Fi radio visible through Windows.Devices.Radios")
        return
    if not turn_on:
        return
    for r in wifi:
        before = RadioState(r.state).name
        t0 = time.perf_counter()
        result = await r.set_state_async(RadioState.ON)
        dt = (time.perf_counter() - t0) * 1000
        print(f"  set_state_async(ON) on {r.name!r}: before={before} result={RadioAccessStatus(result).name} "
              f"after={RadioState(r.state).name} ({dt:.0f} ms)")


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--turn-on", action="store_true", help="ask Windows to switch the Wi-Fi radio ON")
    a = p.parse_args()
    asyncio.run(main(a.turn_on))
