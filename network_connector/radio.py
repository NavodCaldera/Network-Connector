"""Wi-Fi radio state, and turning it ON when the owner clicks "Turn on" (Windows.Devices.Radios).

This module never turns a radio off and never touches Bluetooth or mobile broadband radios.
"""
from __future__ import annotations

import asyncio
import sys
import time
from typing import Any


class RadioError(RuntimeError):
    pass


def _run(coro: Any) -> Any:
    return asyncio.run(coro)


async def _wifi_radios() -> list[Any]:
    from winrt.windows.devices.radios import Radio, RadioAccessStatus, RadioKind

    access = await Radio.request_access_async()
    if access != RadioAccessStatus.ALLOWED:
        raise RadioError("Windows did not allow this app to control the Wi-Fi radio. Use Quick Settings instead.")
    return [r for r in await Radio.get_radios_async() if r.kind == RadioKind.WI_FI]


def wifi_state() -> str:
    """'on', 'off' or 'unknown' (no Wi-Fi radio, not Windows, or the WinRT packages are missing)."""
    if sys.platform != "win32":
        return "unknown"
    try:
        from winrt.windows.devices.radios import RadioState

        radios = _run(_wifi_radios())
    except Exception:  # noqa: BLE001 - shown as "unknown", never an error dialog
        return "unknown"
    if not radios:
        return "unknown"
    return "on" if any(r.state == RadioState.ON for r in radios) else "off"


def turn_wifi_on(wait_s: float = 5.0) -> str:
    """Ask Windows to switch the Wi-Fi radio on. Only call this from the owner's click."""
    if sys.platform != "win32":
        raise RadioError("Turning Wi-Fi on is supported only on Windows.")
    try:
        from winrt.windows.devices.radios import RadioAccessStatus, RadioState
    except ImportError as exc:
        raise RadioError("Install the app extras (pip install network-connector[app]) to turn Wi-Fi on from here.") from exc

    async def work() -> bool:
        radios = await _wifi_radios()
        if not radios:
            raise RadioError("No Wi-Fi radio was found.")
        for radio in radios:
            if radio.state != RadioState.ON:
                if await radio.set_state_async(RadioState.ON) != RadioAccessStatus.ALLOWED:
                    raise RadioError("Windows refused to turn Wi-Fi on. Check airplane mode in Quick Settings.")
        return True

    _run(work())
    deadline = time.monotonic() + wait_s
    while time.monotonic() < deadline:
        if wifi_state() == "on":
            return "Wi-Fi is on."
        time.sleep(0.5)
    raise RadioError("Wi-Fi is still off. Turn it on in Quick Settings, or turn airplane mode off.")
