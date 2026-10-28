r"""Phase 6 spike: read-only Wi-Fi Direct (P2P) device enumeration. Throwaway code.

  spikes\0.3-network\.venv\Scripts\python.exe spikes\0.3-network\p2p_enum_spike.py

Only lists nearby P2P devices / groups. It never pairs, connects or changes Wi-Fi.
"""
import asyncio
import time


async def main() -> None:
    from winrt.windows.devices.wifidirect import WiFiDirectDevice, WiFiDirectDeviceSelectorType
    from winrt.windows.devices.enumeration import DeviceInformation

    print(time.strftime("%Y-%m-%d %H:%M:%S"))
    for kind in (WiFiDirectDeviceSelectorType.ASSOCIATION_ENDPOINT, WiFiDirectDeviceSelectorType.DEVICE_INTERFACE):
        sel = WiFiDirectDevice.get_device_selector(kind)
        t0 = time.perf_counter()
        try:
            found = await asyncio.wait_for(DeviceInformation.find_all_async_aqs_filter(sel), 30)
        except Exception as exc:
            print(f"{kind.name}: failed {type(exc).__name__}: {exc}")
            continue
        print(f"{kind.name}: {len(found)} device(s) in {(time.perf_counter() - t0) * 1000:.0f} ms")
        for d in found:
            p = d.pairing
            print(f"   name={d.name!r} paired={p.is_paired} can_pair={p.can_pair} custom_pairing={p.custom is not None}")


asyncio.run(main())
