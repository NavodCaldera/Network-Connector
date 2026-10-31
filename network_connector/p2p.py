"""Native Wi-Fi Direct (P2P): both PCs keep their normal Wi-Fi and internet.

Unlike the legacy group in wifidirect.py (where the joiner leaves its router to join a Wi-Fi network), here Windows
makes a separate virtual P2P link on each PC. The host advertises and accepts; the joiner finds it, pairs once with
push-button (no PIN), connects, and both sides get an IP address on the link.

All WinRT calls run on one background asyncio loop; callers use the blocking methods.
"""
from __future__ import annotations

import asyncio
import sys
import threading
from dataclasses import dataclass
from typing import Any, Callable

from .wifidirect import DirectLinkError


@dataclass
class Peer:
    name: str
    id: str


def _require_windows() -> None:
    if sys.platform != "win32":
        raise DirectLinkError("Wi-Fi Direct is supported only on Windows.")


def _winrt():
    try:
        import winrt.windows.devices.enumeration as enum
        import winrt.windows.devices.wifidirect as wfd
    except ImportError as exc:
        raise DirectLinkError("Wi-Fi Direct support not installed. Install the 'wifidirect' extra.") from exc
    return wfd, enum


class _Loop:
    """One asyncio loop on a daemon thread (WinRT async calls need a running loop)."""

    def __init__(self) -> None:
        self.loop = asyncio.new_event_loop()
        threading.Thread(target=self.loop.run_forever, name="p2p-loop", daemon=True).start()

    def run(self, coro, timeout: float = 60) -> Any:
        return asyncio.run_coroutine_threadsafe(coro, self.loop).result(timeout)


def _params(wfd: Any, owner_intent: int) -> Any:
    params = wfd.WiFiDirectConnectionParameters()
    params.group_owner_intent = owner_intent
    params.preference_ordered_configuration_methods.append(wfd.WiFiDirectConfigurationMethod.PUSH_BUTTON)
    return params


async def _pair(wfd: Any, enum: Any, info: Any, owner_intent: int) -> None:
    """Pair with push-button, accepting our own side automatically. Already paired is fine."""
    if info.pairing.is_paired:
        return
    custom = info.pairing.custom

    def accept(_sender: Any, args: Any) -> None:
        args.accept()

    token = custom.add_pairing_requested(accept)
    try:
        kinds = enum.DevicePairingKinds.CONFIRM_ONLY | enum.DevicePairingKinds.DISPLAY_PIN | enum.DevicePairingKinds.PROVIDE_PIN
        result = await custom.pair_with_protection_level_and_settings_async(
            kinds, enum.DevicePairingProtectionLevel.DEFAULT, _params(wfd, owner_intent))
    finally:
        custom.remove_pairing_requested(token)
    ok = (enum.DevicePairingResultStatus.PAIRED, enum.DevicePairingResultStatus.ALREADY_PAIRED)
    if result.status not in ok:
        raise DirectLinkError(f"Windows could not pair the two PCs ({result.status.name}). Try again, and click Allow "
                              "if Windows asks.")


def _addresses(device: Any) -> tuple[str, str]:
    pairs = device.get_connection_endpoint_pairs()
    if not pairs or pairs.size == 0:
        raise DirectLinkError("Connected, but Windows gave the link no IP address yet.")
    pair = pairs.get_at(0)
    return pair.local_host_name.display_name, pair.remote_host_name.display_name


class Host:
    """Advertise this PC and accept Wi-Fi Direct connections. on_peer(name, local_ip, remote_ip) per connection."""

    def __init__(self, on_peer: Callable[[str, str, str], None], on_error: Callable[[str], None]) -> None:
        _require_windows()
        self.wfd, self.enum = _winrt()
        self.on_peer, self.on_error = on_peer, on_error
        self._loop = _Loop()
        self._publisher: Any = None
        self._listener: Any = None
        self._token: Any = None
        self._devices: list[Any] = []  # keep connections alive

    def start(self) -> None:
        self._loop.run(self._start(), 15)

    async def _start(self) -> None:
        wfd = self.wfd
        self._publisher = wfd.WiFiDirectAdvertisementPublisher()
        self._publisher.advertisement.listen_state_discoverability = wfd.WiFiDirectAdvertisementListenStateDiscoverability.NORMAL
        self._listener = wfd.WiFiDirectConnectionListener()
        self._token = self._listener.add_connection_requested(self._requested)
        self._publisher.start()
        for _ in range(50):
            if self._publisher.status == wfd.WiFiDirectAdvertisementPublisherStatus.STARTED:
                return
            if self._publisher.status == wfd.WiFiDirectAdvertisementPublisherStatus.ABORTED:
                raise DirectLinkError("Windows aborted Wi-Fi Direct advertising. Wait 10 s and try again.")
            await asyncio.sleep(0.2)
        raise DirectLinkError("Wi-Fi Direct advertising did not start.")

    def _requested(self, _sender: Any, args: Any) -> None:
        request = args.get_connection_request()  # must be taken inside the event
        asyncio.run_coroutine_threadsafe(self._accept(request), self._loop.loop)

    async def _accept(self, request: Any) -> None:
        try:
            info = request.device_information
            await _pair(self.wfd, self.enum, info, owner_intent=15)
            device = await self.wfd.WiFiDirectDevice.from_id_async(info.id)
            self._devices.append(device)
            local, remote = "", ""
            for _ in range(40):  # addresses arrive a moment after the link is up
                try:
                    local, remote = _addresses(device)
                    break
                except DirectLinkError:
                    await asyncio.sleep(0.5)
            self.on_peer(info.name, local, remote)
        except Exception as exc:  # noqa: BLE001 - report, keep listening
            self.on_error(f"Connection from the other PC failed: {exc}")

    def stop(self) -> None:
        async def work() -> None:
            if self._listener is not None and self._token is not None:
                self._listener.remove_connection_requested(self._token)
            if self._publisher is not None:
                self._publisher.stop()
            for device in self._devices:
                device.close()
            self._devices.clear()
        try:
            self._loop.run(work(), 10)
        finally:
            self._loop.loop.call_soon_threadsafe(self._loop.loop.stop)


class Joiner:
    """Find advertising PCs, connect to one, and learn the host's address on the link."""

    def __init__(self) -> None:
        _require_windows()
        self.wfd, self.enum = _winrt()
        self._loop = _Loop()
        self._device: Any = None

    def find(self, seconds: float = 15) -> list[Peer]:
        async def work() -> list[Peer]:
            selector = self.wfd.WiFiDirectDevice.get_device_selector(self.wfd.WiFiDirectDeviceSelectorType.ASSOCIATION_ENDPOINT)
            found: dict[str, Peer] = {}
            loop = asyncio.get_running_loop()
            end = loop.time() + seconds
            while loop.time() < end:  # devices show up over a few seconds
                for info in await self.enum.DeviceInformation.find_all_async_aqs_filter(selector):
                    found.setdefault(info.id, Peer(info.name or "(no name)", info.id))
                if found:
                    await asyncio.sleep(2)  # one more round to catch the rest
                    for info in await self.enum.DeviceInformation.find_all_async_aqs_filter(selector):
                        found.setdefault(info.id, Peer(info.name or "(no name)", info.id))
                    break
                await asyncio.sleep(2)
            return sorted(found.values(), key=lambda p: p.name.lower())
        return self._loop.run(work(), seconds + 30)

    def connect(self, peer: Peer) -> tuple[str, str]:
        """Pair (once) and connect. Returns (this PC's address, the host's address) on the link."""
        async def work() -> tuple[str, str]:
            info = await self.enum.DeviceInformation.create_from_id_async(peer.id)
            await _pair(self.wfd, self.enum, info, owner_intent=0)
            self._device = await self.wfd.WiFiDirectDevice.from_id_async(peer.id)
            for _ in range(40):
                try:
                    return _addresses(self._device)
                except DirectLinkError:
                    await asyncio.sleep(0.5)
            raise DirectLinkError("Connected, but no IP address appeared on the link.")
        return self._loop.run(work(), 90)

    def close(self) -> None:
        async def work() -> None:
            if self._device is not None:
                self._device.close()
                self._device = None
        try:
            self._loop.run(work(), 10)
        finally:
            self._loop.loop.call_soon_threadsafe(self._loop.loop.stop)
