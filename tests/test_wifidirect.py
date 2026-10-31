"""Hardware-free checks for Windows API handling and outbound discovery."""
import ipaddress
import subprocess
import time
import threading
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from network_connector import wifidirect as direct


@pytest.mark.parametrize("ssid,password", [("-option", "abcdefgh"), ("x\n", "abcdefgh"),
    ("a" * 33, "abcdefgh"), ("safe", "short"), ("safe", "password<&"), ("x;echo", "abcdefgh")])
def test_credentials_reject_unsafe_input_before_network_commands(monkeypatch, ssid, password):
    run = Mock()
    monkeypatch.setattr(direct, "_run", run)
    with pytest.raises(direct.DirectLinkError):
        direct.join(ssid, password)
    run.assert_not_called()


def test_join_argument_lists_protected_profile_and_cleanup(monkeypatch):
    calls, profile = [], []
    protect = Mock()
    monkeypatch.setattr(direct, "protect_path", protect)
    monkeypatch.setattr(direct.sys, "platform", "win32")
    iface = direct.InterfaceInfo("Wi-Fi", "adapter", "10.23.4.9", "10.23.4.0/24", "10.23.4.1")
    monkeypatch.setattr(direct, "direct_interfaces", lambda ssid=None: [iface])
    def run(args, timeout=20):
        calls.append(args)
        if "add" in args:
            path = Path(next(a.removeprefix("filename=") for a in args if a.startswith("filename=")))
            profile.append(path)
            protect.assert_called_once_with(path)
            assert "secretABC123!" in path.read_text()
            assert "<connectionMode>auto</connectionMode>" in path.read_text()
        return subprocess.CompletedProcess(args, 0, "", "")
    monkeypatch.setattr(direct, "_run", run)
    assert direct.join("DIRECT-NC-1234", "secretABC123!") is iface
    assert not profile[0].exists()
    assert calls[1] == ["netsh", "wlan", "connect", "name=DIRECT-NC-1234", "ssid=DIRECT-NC-1234"]
    assert all("secretABC123!" not in argument for call in calls for argument in call)


def test_profile_deleted_when_windows_rejects_it(monkeypatch):
    paths = []
    monkeypatch.setattr(direct, "protect_path", lambda _: None)
    def run(args):
        paths.append(Path(args[4].removeprefix("filename=")))
        return subprocess.CompletedProcess(args, 1, "", "")
    monkeypatch.setattr(direct, "_run", run)
    with pytest.raises(direct.DirectLinkError, match="install"):
        direct.join("DIRECT-NC-1234", "secretABC123!")
    assert not paths[0].exists()


def test_subprocess_has_timeout_and_no_shell(monkeypatch):
    run = Mock(return_value=subprocess.CompletedProcess([], 0, "", ""))
    monkeypatch.setattr(direct.subprocess, "run", run)
    direct._run(["netsh", "wlan", "show", "interfaces"], timeout=3)
    assert run.call_args.args[0] == ["netsh", "wlan", "show", "interfaces"]
    assert run.call_args.kwargs["timeout"] == 3
    assert not run.call_args.kwargs.get("shell", False)


def test_adapter_discovery_uses_actual_prefix_gateway_and_filters_peers(monkeypatch):
    monkeypatch.setattr(direct, "_adapter_rows", lambda: [
        {"name": "direct", "description": "Microsoft Wi-Fi Direct Virtual Adapter #3", "up": True,
         "ips": [{"ip": "169.254.12.6", "prefix": 16}], "gateway": "", "category": "Public",
         "peers": ["169.254.12.9", "224.0.0.1", "169.254.12.6", "not-ip"]},
        {"name": "wifi", "description": "Intel WLAN", "network_name": "DIRECT-NC-1234", "up": True,
         "ips": [{"ip": "10.23.4.9", "prefix": 24}], "gateway": "10.23.4.7"},
        {"name": "lan", "description": "Ethernet", "up": True, "ips": [{"ip": "10.0.0.2", "prefix": 24}]},
    ])
    owner, worker = direct.direct_interfaces()
    assert owner.ip == "169.254.12.6" and owner.subnet == "169.254.0.0/16" and owner.is_owner
    assert owner.peers == ["169.254.12.9"]
    assert direct.candidate_controllers(worker) == ["10.23.4.7", "10.23.4.1"]
    assert direct.candidate_controllers(None)  # no subnet scan, only a bounded gateway/.1 probe


def test_existing_hotspot_is_neither_adopted_nor_reported(monkeypatch):
    iface = direct.InterfaceInfo("hotspot", "Wi-Fi Direct adapter", "192.168.137.1", "192.168.137.0/24", is_owner=True)
    monkeypatch.setattr(direct, "find_direct_interface", lambda: iface)
    factory = Mock(side_effect=AssertionError("must not start a publisher over an existing group"))
    monkeypatch.setattr(direct, "_create_publisher", factory)
    group = direct.DirectGroup()
    info = group.status()
    assert info.mode == "none" and not info.running and not info.ip
    with pytest.raises(direct.DirectLinkError, match="already running"):
        group.start()
    factory.assert_not_called()


def test_publisher_retained_on_thread_and_stopped(monkeypatch):
    monkeypatch.setattr(direct, "find_direct_interface", lambda: None)
    statuses = SimpleNamespace(STARTED=1, ABORTED=2)
    publisher = Mock(status=1)
    acceptor = Mock()
    monkeypatch.setattr(direct, "_ConnectionAcceptor", lambda on_error: acceptor)
    monkeypatch.setattr(direct, "_create_publisher", lambda ssid, password: (publisher, statuses))
    group = direct.DirectGroup()
    try:
        info = group.start()
        assert info.running and info.mode == "A" and len(info.passphrase) == 16
        assert "passphrase" not in repr(info)
        assert group._publisher is publisher and group._thread.is_alive()
        assert group.start().ssid == info.ssid
    finally:
        group.stop()
    publisher.start.assert_called_once()
    publisher.stop.assert_called_once()
    acceptor.close.assert_called_once()
    assert not group._thread.is_alive()


def test_missing_api_failure_is_clear_and_leaks_no_password(monkeypatch):
    monkeypatch.setattr(direct, "find_direct_interface", lambda: None)
    monkeypatch.setattr(direct, "_create_publisher", Mock(side_effect=ValueError("secretABC123!")))
    group = direct.DirectGroup()
    with pytest.raises(direct.DirectLinkError, match="API failed") as error:
        group.start("DIRECT-NC-1234", "secretABC123!")
    assert "secretABC123!" not in str(error.value)
    assert not group._thread.is_alive()


def test_connection_acceptor_retains_device_until_disconnect():
    acceptor = direct._ConnectionAcceptor.__new__(direct._ConnectionAcceptor)
    acceptor._lock = threading.RLock()
    acceptor._closed = False
    acceptor._pending, acceptor._devices = {}, {}
    acceptor._completed, acceptor._disconnected = 1, 2
    acceptor._on_error = Mock()
    device = Mock(connection_status=1)
    operation = SimpleNamespace(get_results=lambda: device)
    acceptor._device_api = SimpleNamespace(from_id_async=lambda ident: operation)
    request = Mock(device_information=SimpleNamespace(id="peer-id"))
    acceptor._requested(None, SimpleNamespace(get_connection_request=lambda: request))
    assert len(acceptor._pending) == 1
    operation.completed(operation, 1)
    assert acceptor._devices["peer-id"][0] is device
    assert not acceptor._pending
    request.close.assert_called_once()
    device.close.assert_not_called()
    disconnected = device.add_connection_status_changed.call_args.args[0]
    device.connection_status = 2
    disconnected(device, None)
    assert not acceptor._devices
    device.close.assert_called_once()
    acceptor._on_error.assert_not_called()


def test_connection_acceptor_stop_cancels_pending_and_closes_devices():
    acceptor = direct._ConnectionAcceptor.__new__(direct._ConnectionAcceptor)
    acceptor._lock = threading.RLock()
    acceptor._closed = False
    operation, request, device = Mock(), Mock(), Mock()
    acceptor._pending, acceptor._devices = {1: (operation, request)}, {"peer": (device, "device-token")}
    acceptor._listener, acceptor._token = Mock(), "listener-token"
    acceptor.close()
    assert acceptor._closed and not acceptor._pending and not acceptor._devices
    operation.cancel.assert_called_once()
    request.close.assert_called_once()
    device.remove_connection_status_changed.assert_called_once_with("device-token")
    device.close.assert_called_once()
