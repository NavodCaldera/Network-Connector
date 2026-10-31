"""Optional Windows Wi-Fi Direct and outbound-only controller discovery.

Adapter behaviour must still be checked on the actual laptops. WinRT is imported only
when starting a group. Windows Mobile Hotspot is not used: the app runs its own group only.
"""
from __future__ import annotations

import ipaddress
import json
import re
import secrets
import string
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from xml.sax.saxutils import escape

from .security import protect_path


class DirectLinkError(RuntimeError):
    """The adapter, Windows service or optional API could not make a direct link."""


@dataclass
class InterfaceInfo:
    name: str
    description: str
    ip: str
    subnet: str
    gateway: str = ""
    category: str = ""
    peers: list[str] = field(default_factory=list)
    is_owner: bool = False


@dataclass
class GroupInfo:
    mode: str = "none"
    ssid: str = ""
    passphrase: str = field(default="", repr=False)
    interface: str = ""
    ip: str = ""
    subnet: str = ""
    peers: list[str] = field(default_factory=list)
    running: bool = False
    category: str = ""
    status: str = "Stopped"
    error: str = ""


_adapter_lock = threading.Lock()
_adapter_cache: list[dict[str, Any]] = []
_adapter_cache_time = 0.0


def validate_credentials(ssid: str, passphrase: str | None = None) -> None:
    if not isinstance(ssid, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 _.-]{0,31}", ssid):
        raise DirectLinkError("SSID must be 1–32 letters, digits, spaces, dots, underscores or hyphens, starting with a letter or digit.")
    if passphrase is not None and (not isinstance(passphrase, str) or not re.fullmatch(r"[A-Za-z0-9!@#%+=_.-]{8,63}", passphrase)):
        raise DirectLinkError("Passphrase must be 8–63 letters, digits or !@#%+=_.- characters.")


def _run(args: list[str], timeout: float = 20) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(args, capture_output=True, text=True, errors="replace", timeout=timeout,
                              creationflags=0x08000000 if sys.platform == "win32" else 0)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise DirectLinkError("Windows networking command failed or timed out. Check the Wi-Fi adapter and WLAN AutoConfig service.") from exc


def _adapter_rows() -> list[dict[str, Any]]:
    global _adapter_cache, _adapter_cache_time
    with _adapter_lock:
        if time.monotonic() - _adapter_cache_time < 2:
            return _adapter_cache
        _adapter_cache = _read_adapter_rows()
        _adapter_cache_time = time.monotonic()
        return _adapter_cache


def _read_adapter_rows() -> list[dict[str, Any]]:
    if sys.platform != "win32":
        return []
    # This command is constant: no SSID, address or credential is interpolated into PowerShell.
    # One bulk query per cmdlet, joined by ifIndex: per-adapter Get-NetIPConfiguration took ~12 s and threw for
    # hidden adapters without an IP interface. A failing cmdlet leaves its field empty instead of losing every row.
    command = r"""$ErrorActionPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.Encoding]::UTF8;
$ips=@(Get-NetIPAddress -AddressFamily IPv4); $routes=@(Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0');
$profiles=@(Get-NetConnectionProfile);
$neigh=@(Get-NetNeighbor -AddressFamily IPv4 | Where-Object {$_.State -in @('Reachable','Stale','Delay','Probe','Permanent')});
$rows=@(Get-NetAdapter -IncludeHidden | ForEach-Object {
 $i=$_.ifIndex; $p=@($profiles | Where-Object InterfaceIndex -eq $i)[0];
 @{name=$_.Name;description=$_.InterfaceDescription;up=($_.Status -eq 'Up');
 ips=@($ips | Where-Object InterfaceIndex -eq $i | ForEach-Object {@{ip=$_.IPAddress;prefix=$_.PrefixLength}});
 gateway=[string](@($routes | Where-Object InterfaceIndex -eq $i | ForEach-Object NextHop)[0]);
 category=[string]$p.NetworkCategory;network_name=[string]$p.Name;
 peers=@($neigh | Where-Object InterfaceIndex -eq $i | ForEach-Object IPAddress)}
}); ConvertTo-Json -InputObject $rows -Depth 5 -Compress"""
    proc = _run(["powershell", "-NoProfile", "-NonInteractive", "-Command", command], timeout=15)
    if proc.returncode:
        return []
    try:
        rows = json.loads(proc.stdout.lstrip("\ufeff"))
    except (ValueError, TypeError):
        return []
    return rows if isinstance(rows, list) else [rows] if isinstance(rows, dict) else []


def direct_interfaces(ssid: str | None = None) -> list[InterfaceInfo]:
    found = []
    for row in _adapter_rows():
        if not row.get("up"):
            continue
        description = str(row.get("description", ""))
        direct = "wi-fi direct" in description.lower() or "wifi direct" in description.lower()
        for address in row.get("ips", []):
            try:
                ip = ipaddress.IPv4Address(address["ip"])
                network = ipaddress.IPv4Network(f"{ip}/{int(address['prefix'])}", strict=False)
            except (KeyError, TypeError, ValueError):
                continue
            network_name = str(row.get("network_name", ""))
            direct_client = network_name.startswith("DIRECT-") or (ssid is not None and network_name == ssid)
            if ip.is_loopback or ip.is_unspecified or (not direct and not direct_client and ip not in ipaddress.IPv4Network("192.168.137.0/24")):
                continue
            if not (ip.is_private or ip.is_link_local):
                continue
            gateway = str(row.get("gateway", ""))
            try:
                if gateway and ipaddress.IPv4Address(gateway) not in network:
                    gateway = ""
            except ValueError:
                gateway = ""
            peers = []
            for raw in row.get("peers", []):
                try:
                    peer = ipaddress.IPv4Address(raw)
                except ValueError:
                    continue
                if peer in network and peer not in (ip, network.network_address, network.broadcast_address) and not peer.is_multicast:
                    peers.append(str(peer))
            owner = (direct and not gateway) or (ip == network.network_address + 1 and gateway in ("", str(ip)))
            found.append(InterfaceInfo(str(row.get("name", "")), description, str(ip), str(network),
                                       gateway, str(row.get("category", "")), peers[:64], owner))
    return found


def find_direct_interface() -> InterfaceInfo | None:
    interfaces = direct_interfaces()
    # Prefer the group-owner adapter when multiple direct interfaces are present.
    return next((i for i in interfaces if i.is_owner), interfaces[0] if interfaces else None)


def candidate_controllers(iface: InterfaceInfo | None = None) -> list[str]:
    iface = iface or find_direct_interface()
    if iface is None:
        return []
    try:
        network = ipaddress.IPv4Network(iface.subnet, strict=False)
    except ValueError:
        return []
    candidates = [iface.gateway, str(network.network_address + 1)]
    out = []
    for host in candidates:
        try:
            ip = ipaddress.IPv4Address(host)
        except ValueError:
            continue
        if ip in network and ip not in (network.network_address, network.broadcast_address) and str(ip) != iface.ip and host not in out:
            out.append(str(ip))
    return out


def probe(host: str, port: int, timeout: float = 2) -> str | None:
    from .client import probe as cert_probe

    return cert_probe(host, port, timeout)


def join(ssid: str, passphrase: str) -> InterfaceInfo:
    validate_credentials(ssid, passphrase)
    if sys.platform != "win32":
        raise DirectLinkError("Joining Wi-Fi networks is supported only on Windows.")
    xml = f'''<?xml version="1.0" encoding="UTF-8"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{escape(ssid)}</name>
<SSIDConfig><SSID><name>{escape(ssid)}</name></SSID></SSIDConfig><connectionType>ESS</connectionType>
<connectionMode>auto</connectionMode><MSM><security><authEncryption><authentication>WPA2PSK</authentication>
<encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType>
<protected>false</protected><keyMaterial>{escape(passphrase)}</keyMaterial></sharedKey></security></MSM></WLANProfile>'''
    # Protect the empty file before writing any secret. Its contents never enter a command line.
    with tempfile.TemporaryDirectory(prefix="nc-wifi-") as directory:
        path = Path(directory) / "profile.xml"
        path.touch()
        protect_path(path)
        path.write_text(xml, encoding="utf-8")
        result = _run(["netsh", "wlan", "add", "profile", f"filename={path}", "user=current"])
        if result.returncode:
            raise DirectLinkError("Windows could not install the Wi-Fi profile. Check WLAN AutoConfig and your adapter.")
    result = _run(["netsh", "wlan", "connect", f"name={ssid}", f"ssid={ssid}"])
    if result.returncode:
        raise DirectLinkError("Windows could not join this Wi-Fi network. Check its SSID and password.")
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        interfaces = direct_interfaces(ssid)
        iface = next((i for i in interfaces if not i.is_owner), None)
        if iface:
            return iface
        time.sleep(1)
    raise DirectLinkError("Windows accepted the connection request, but no direct-link IP address appeared. Check the connection in Windows Wi-Fi settings.")


def leave(ssid: str) -> None:
    validate_credentials(ssid)
    if sys.platform != "win32":
        raise DirectLinkError("Leaving Wi-Fi networks is supported only on Windows.")
    # Removing only this saved profile also disconnects it, without disconnecting an unrelated WLAN.
    result = _run(["netsh", "wlan", "delete", "profile", f"name={ssid}"])
    if result.returncode:
        raise DirectLinkError("Windows could not remove this Wi-Fi profile.")


def _create_publisher(ssid: str, passphrase: str) -> tuple[Any, Any]:
    try:
        from winrt.windows.devices.wifidirect import WiFiDirectAdvertisementPublisher, WiFiDirectAdvertisementPublisherStatus
        from winrt.windows.security.credentials import PasswordCredential
    except ImportError as exc:
        raise DirectLinkError("Wi-Fi Direct support not installed. Install the 'wifidirect' extra and try again.") from exc
    publisher = WiFiDirectAdvertisementPublisher()
    publisher.advertisement.is_autonomous_group_owner_enabled = True
    legacy = publisher.advertisement.legacy_settings
    legacy.is_enabled = True
    legacy.ssid = ssid
    credential = PasswordCredential()
    credential.password = passphrase
    legacy.passphrase = credential
    return publisher, WiFiDirectAdvertisementPublisherStatus


def _initialize_apartment() -> Any:
    try:
        from winrt.runtime import ApartmentType, init_apartment, uninit_apartment
    except ImportError:
        return None  # _create_publisher supplies the user-facing optional-dependency error.
    init_apartment(ApartmentType.MULTI_THREADED)
    return uninit_apartment


class _ConnectionAcceptor:
    """Accept legacy clients and retain their OS connection objects until disconnect."""

    def __init__(self, on_error: Any) -> None:
        from winrt.windows.devices.wifidirect import WiFiDirectConnectionListener, WiFiDirectDevice, WiFiDirectConnectionStatus
        from winrt.windows.foundation import AsyncStatus

        self._listener = WiFiDirectConnectionListener()
        self._device_api = WiFiDirectDevice
        self._disconnected = WiFiDirectConnectionStatus.DISCONNECTED
        self._completed = AsyncStatus.COMPLETED
        self._on_error = on_error
        self._lock = threading.RLock()
        self._closed = False
        self._pending: dict[int, tuple[Any, Any]] = {}
        self._devices: dict[str, tuple[Any, Any]] = {}
        self._token = self._listener.add_connection_requested(self._requested)

    @staticmethod
    def _close(value: Any) -> None:
        try:
            value.close()
        except Exception:
            pass

    def _requested(self, sender: Any, args: Any) -> None:
        request = None
        operation = None
        try:
            request = args.get_connection_request()
            with self._lock:
                if self._closed or len(self._pending) + len(self._devices) >= 64:
                    self._close(request)
                    return
                device_id = request.device_information.id
                operation = self._device_api.from_id_async(device_id)
                key = id(operation)
                self._pending[key] = operation, request

            def completed(op: Any, status: Any) -> None:
                device = None
                try:
                    if status != self._completed:
                        return
                    device = op.get_results()

                    def disconnected(value: Any, unused: Any) -> None:
                        if value.connection_status == self._disconnected:
                            with self._lock:
                                saved = self._devices.pop(device_id, None)
                            if saved:
                                try:
                                    saved[0].remove_connection_status_changed(saved[1])
                                except Exception:
                                    pass
                                self._close(saved[0])

                    with self._lock:
                        if self._closed:
                            self._close(device)
                            return
                        token = device.add_connection_status_changed(disconnected)
                        previous = self._devices.pop(device_id, None)
                        self._devices[device_id] = device, token
                        if previous:
                            try:
                                previous[0].remove_connection_status_changed(previous[1])
                            except Exception:
                                pass
                            self._close(previous[0])
                except Exception:
                    if device is not None:
                        self._close(device)
                    self._on_error()
                finally:
                    with self._lock:
                        self._pending.pop(key, None)
                    self._close(request)

            operation.completed = completed
        except Exception:
            if operation is not None:
                with self._lock:
                    self._pending.pop(id(operation), None)
                try:
                    operation.cancel()
                except Exception:
                    pass
            if request is not None:
                self._close(request)
            self._on_error()

    def close(self) -> None:
        with self._lock:
            self._closed = True
            pending, self._pending = list(self._pending.values()), {}
            devices, self._devices = list(self._devices.values()), {}
        try:
            self._listener.remove_connection_requested(self._token)
        except Exception:
            pass
        for operation, request in pending:
            try:
                operation.cancel()
            except Exception:
                pass
            self._close(request)
        for device, token in devices:
            try:
                device.remove_connection_status_changed(token)
            except Exception:
                pass
            self._close(device)


class DirectGroup:
    """Keeps the WinRT publisher on a dedicated, retained background thread."""

    def __init__(self) -> None:
        self._info = GroupInfo()
        self._lock = threading.RLock()
        self._operation = threading.Lock()
        self._stop = threading.Event()
        self._ready = threading.Event()
        self._thread: threading.Thread | None = None
        self._publisher: Any = None
        self._acceptor: Any = None

    def start(self, ssid: str | None = None, passphrase: str | None = None) -> GroupInfo:
        if ssid is not None:
            validate_credentials(ssid, passphrase)
        elif passphrase is not None:
            validate_credentials("DIRECT-NC-test", passphrase)
        with self._operation:
            if self._thread and self._thread.is_alive():
                return self.status()
            iface = find_direct_interface()
            if iface and iface.is_owner:
                # Something else (for example Windows Mobile Hotspot) already owns the adapter; we never adopt it.
                raise DirectLinkError("Another Wi-Fi Direct group or Mobile Hotspot is already running on this PC. "
                                      "Turn it off in Windows Settings, then start the group again.")
            if sys.platform != "win32":
                raise DirectLinkError("Wi-Fi Direct is supported only on Windows.")
            ssid = ssid or "DIRECT-NC-" + "".join(secrets.choice(string.ascii_uppercase + string.digits) for _ in range(4))
            passphrase = passphrase or "".join(secrets.choice(string.ascii_letters + string.digits) for _ in range(16))
            self._stop.clear()
            self._ready.clear()
            with self._lock:
                self._info = GroupInfo(mode="A", ssid=ssid, passphrase=passphrase, status="Starting")
            self._thread = threading.Thread(target=self._publish, args=(ssid, passphrase), name="wifi-direct-publisher", daemon=True)
            self._thread.start()
            if not self._ready.wait(12):
                self._stop.set()
                self._thread.join(timeout=5)
                raise DirectLinkError("Wi-Fi Direct start timed out. Check that the adapter supports Wi-Fi Direct and the radio is on.")
            with self._lock:
                error, running = self._info.error, self._info.running
            if error and not running:
                self._thread.join(timeout=5)
                raise DirectLinkError(error)
            return self.status()

    def _publish(self, ssid: str, passphrase: str) -> None:
        publisher = None
        acceptor = None
        uninitialize = None
        token = None
        try:
            uninitialize = _initialize_apartment()
            publisher, statuses = _create_publisher(ssid, passphrase)
            self._publisher = publisher

            def connection_failed() -> None:
                with self._lock:
                    self._info.error = "Windows could not accept a Wi-Fi connection; retry joining."

            acceptor = _ConnectionAcceptor(connection_failed)
            self._acceptor = acceptor

            def changed(sender: Any, args: Any) -> None:
                with self._lock:
                    if args.status == statuses.STARTED:
                        self._info.running, self._info.status = True, "Group advertised"
                        self._ready.set()
                    elif args.status == statuses.ABORTED:
                        self._info.running = False
                        self._info.error = "Windows aborted Wi-Fi Direct. Check adapter support, radio state and permissions."
                        self._ready.set()
                        self._stop.set()
                    elif args.status == getattr(statuses, "STOPPED", -1):
                        self._info.running = False
                        if not self._stop.is_set():
                            self._info.error = "Windows stopped the Wi-Fi Direct group. Check the radio and adapter."
                        self._ready.set()
                        self._stop.set()

            token = publisher.add_status_changed(changed)
            publisher.start()
            deadline = time.monotonic() + 10
            while not self._stop.wait(0.2):
                status = publisher.status
                if status == statuses.STARTED:
                    with self._lock:
                        self._info.running, self._info.status = True, "Group advertised"
                    self._ready.set()
                elif status == statuses.ABORTED:
                    raise DirectLinkError("Windows aborted Wi-Fi Direct. Check adapter support and permissions.")
                elif status == getattr(statuses, "STOPPED", -1):
                    raise DirectLinkError("Windows stopped the Wi-Fi Direct group. Check the radio and adapter.")
                if not self._ready.is_set() and time.monotonic() >= deadline:
                    raise DirectLinkError("The Wi-Fi Direct publisher did not start. Check adapter support and the radio.")
        except DirectLinkError as exc:
            with self._lock:
                self._info.error, self._info.running = str(exc), False
        except Exception as exc:
            # Do not expose API exception text: it might contain a credential supplied to Windows.
            with self._lock:
                self._info.error = f"Wi-Fi Direct API failed ({type(exc).__name__}). Check permissions and adapter support."
                self._info.running = False
        finally:
            if acceptor is not None:
                acceptor.close()
            self._acceptor = None
            if publisher is not None:
                try:
                    publisher.stop()
                    if token is not None:
                        publisher.remove_status_changed(token)
                except Exception:
                    pass
            self._publisher = None
            publisher = acceptor = None
            if uninitialize:
                try:
                    uninitialize()
                except Exception:
                    pass
            with self._lock:
                self._info.running = False
            self._ready.set()

    def stop(self) -> None:
        with self._operation:
            self._stop.set()
            if self._thread and self._thread is not threading.current_thread():
                self._thread.join(timeout=10)
                if self._thread.is_alive():
                    raise DirectLinkError("Windows has not released the Wi-Fi Direct publisher yet.")
            with self._lock:
                self._info = GroupInfo(status="Stopped")

    def status(self) -> GroupInfo:
        iface = find_direct_interface()
        with self._lock:
            info = GroupInfo(**vars(self._info))
        # Only our own group's adapter counts; a hotspot started elsewhere is never reported or listened on.
        if iface and info.mode == "A":
            info.interface, info.ip, info.subnet = iface.name, iface.ip, iface.subnet
            info.category, info.peers = iface.category, list(iface.peers)
        return info


# ------------------------------------------------------------------ open groups without a router (PLAN §15.3)
DIRECT_NC_RE = re.compile(r"^DIRECT-NC-[A-Z0-9]{4}$")
# Not a secret: every copy of the app derives the same passphrase, so anyone with the app can join in one
# click. The Wi-Fi password is not the security boundary (sandbox, signatures, TLS and result checks are).
_APP_SALT = b"network-connector/open-direct-group/v1"
SCAN_INTERVAL_S = 30.0


def open_group_ssid(device_id: str) -> str:
    if not re.fullmatch(r"[0-9a-f]{64}", device_id or ""):
        raise DirectLinkError("A full device ID is required for an open direct group.")
    return f"DIRECT-NC-{device_id[:4].upper()}"


def derived_passphrase(ssid: str) -> str:
    """PBKDF2(ssid, app salt): the same on every copy of the app, 24 URL-safe characters."""
    import base64
    import hashlib

    if not DIRECT_NC_RE.fullmatch(ssid or ""):
        raise DirectLinkError("Only DIRECT-NC-XXXX groups use the derived passphrase.")
    raw = hashlib.pbkdf2_hmac("sha256", ssid.encode("ascii"), _APP_SALT, 100_000, dklen=18)
    return base64.urlsafe_b64encode(raw).decode("ascii")


def connected_wlan_ssids() -> list[str] | None:
    """SSIDs this PC's Wi-Fi is connected to now (WinRT NetworkInformation, not localized netsh text).
    None when it can't be told; callers then assume "connected" and never join by themselves."""
    if sys.platform != "win32":
        return None
    try:
        from winrt.windows.networking.connectivity import NetworkInformation

        out = []
        for profile in NetworkInformation.get_connection_profiles():
            if profile.is_wlan_connection_profile:
                ssid = profile.wlan_connection_profile_details.get_connected_ssid()
                if ssid:
                    out.append(str(ssid))
        return out
    except Exception:  # noqa: BLE001 - missing package or API error: unknown
        return None


def scan_direct_groups() -> list[str]:
    """Visible DIRECT-NC-XXXX networks (WinRT WiFiAdapter scan). Empty when scanning is not possible."""
    if sys.platform != "win32":
        return []
    import asyncio

    async def work() -> list[str]:
        from winrt.windows.devices.wifi import WiFiAccessStatus, WiFiAdapter

        if await WiFiAdapter.request_access_async() != WiFiAccessStatus.ALLOWED:
            raise DirectLinkError("Windows did not allow a Wi-Fi scan. Check location access in Privacy settings.")
        found: set[str] = set()
        for adapter in await WiFiAdapter.find_all_adapters_async():
            await adapter.scan_async()
            found.update(n.ssid for n in adapter.network_report.available_networks if DIRECT_NC_RE.fullmatch(n.ssid or ""))
        return sorted(found)

    try:
        return asyncio.run(work())
    except DirectLinkError:
        raise
    except Exception:  # noqa: BLE001
        return []


def join_open_group(ssid: str) -> InterfaceInfo:
    """One-click join of an open DIRECT-NC-XXXX group with the derived passphrase. leave(ssid) deletes the profile."""
    return join(ssid, derived_passphrase(ssid))


@dataclass
class AutoJoin:
    """When may a helper join a nearby DIRECT-NC group by itself? (Phase 6 report §2.)

    - Only when it is on no Wi-Fi network at all: the i7's adapter supports no second station, so
      joining would drop the owner's home Wi-Fi. Unknown connection state counts as connected.
    - While on Wi-Fi it only *offers* the join: "Join (disconnects <SSID>)".
    - Scans at most every 30 s.
    """
    interval_s: float = SCAN_INTERVAL_S
    _last_scan: float = -1e9

    def due(self, now: float) -> bool:
        if now - self._last_scan < self.interval_s:
            return False
        self._last_scan = now
        return True

    @staticmethod
    def decide(visible: list[str], connected: list[str] | None, own_ssid: str = "") -> tuple[str, str, str] | None:
        """('join', ssid, '') / ('offer', ssid, current SSID) / None."""
        groups = [s for s in visible if DIRECT_NC_RE.fullmatch(s) and s != own_ssid]
        if not groups:
            return None
        if connected is None:
            return ("offer", groups[0], "your current Wi-Fi")
        if any(DIRECT_NC_RE.fullmatch(s) for s in connected):
            return None  # already on a group
        if connected:
            return ("offer", groups[0], connected[0])
        return ("join", groups[0], "")
