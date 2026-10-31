from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from network_connector import client, gui, localworker
from network_connector.config import ControllerConfig, WorkerConfig, save_config
from network_connector.controller import ControllerRuntime, port_in_use_message


def test_local_pairing_reuses_loopback_despite_lan_change(monkeypatch, tmp_path):
    cfg = WorkerConfig(controller_host="127.0.0.1", controller_port=9876, token="keep")
    monkeypatch.setattr(localworker, "local_worker_dir", lambda: tmp_path)
    monkeypatch.setattr(localworker, "load_worker_config", lambda _: cfg)
    admin = Mock()
    assert localworker.ensure_paired(admin, "192.168.1.20", 9876) is cfg
    admin.create_pairing.assert_not_called()


def test_admin_client_from_disk_uses_loopback(monkeypatch, tmp_path):
    save_config(ControllerConfig(bind_host="192.168.1.20", port=9876), tmp_path / "controller.json")
    (tmp_path / "admin.token").write_text("test-token")
    factory = Mock()
    monkeypatch.setattr(client, "AdminClient", factory)
    client.admin_client_from_disk(tmp_path)
    assert factory.call_args.args[:2] == ("127.0.0.1", 9876)


def test_runtime_also_listens_on_loopback(tmp_path):
    from conftest import free_port
    cfg = ControllerConfig(bind_host="127.0.0.2", port=free_port())
    rt = ControllerRuntime(cfg, tmp_path)
    try:
        rt.start()
        with_client = client.AdminClient("127.0.0.1", cfg.port, rt.cert, rt.admin_token)
        try:
            assert with_client.summary()["version"]
            assert len(rt._servers) == 2
        finally:
            with_client.close()
    finally:
        rt.stop()
        rt.store.close()


def test_port_error_includes_process(monkeypatch):
    import psutil
    monkeypatch.setattr(psutil, "net_connections", lambda **_: [
        SimpleNamespace(status=psutil.CONN_LISTEN, laddr=SimpleNamespace(port=8765), pid=42)])
    monkeypatch.setattr(psutil, "Process", lambda _: SimpleNamespace(name=lambda: "example.exe"))
    assert "PID 42, name example.exe" in port_in_use_message(8765)


def test_port_error_ignores_own_listeners_and_names_missing_address(monkeypatch):
    import os

    import psutil
    monkeypatch.setattr(psutil, "net_connections", lambda **_: [
        SimpleNamespace(status=psutil.CONN_LISTEN, laddr=SimpleNamespace(ip="127.0.0.1", port=8765), pid=os.getpid())])
    monkeypatch.setattr(psutil, "net_if_addrs", lambda: {})
    message = port_in_use_message(8765, "192.168.137.1")
    assert "another program" not in message and "no longer has the address 192.168.137.1" in message


def test_start_skips_extra_address_this_pc_no_longer_has(tmp_path, monkeypatch):
    from conftest import free_port

    from network_connector import controller as controller_module

    cfg = ControllerConfig(bind_host="127.0.0.1", bind_hosts=["127.0.0.1", "192.168.137.1"], port=free_port())
    rt = ControllerRuntime(cfg, tmp_path)
    monkeypatch.setattr(controller_module, "_host_assigned", lambda host: host == "127.0.0.1")
    try:
        rt.start()
        assert rt.listener_hosts == ["127.0.0.1"]
        assert "192.168.137.1" in rt.cfg.bind_hosts  # remembered for when the direct group is back
    finally:
        rt.stop()
        rt.store.close()


def test_gui_attaches_without_starting_runtime(monkeypatch, tmp_path):
    save_config(ControllerConfig(bind_host="192.168.1.20"), tmp_path / "controller.json")
    admin = Mock()
    monkeypatch.setattr(gui, "admin_client_from_disk", lambda _: admin)
    import network_connector.controller as controller
    factory = Mock()
    monkeypatch.setattr(controller, "ControllerRuntime", factory)
    app = gui.ControllerApp.__new__(gui.ControllerApp)
    app.admin = app.runtime = None
    app.cfg_path = tmp_path / "controller.json"
    app.bind_var = SimpleNamespace(get=lambda: "192.168.1.20")
    app.port_var = SimpleNamespace(get=lambda: 8765)
    app.start_btn, app.status, app.local_btn = Mock(), Mock(), Mock()
    app.bg = SimpleNamespace(run=lambda work, done, fail: done(work()))
    app.toggle_controller()
    assert app.admin is admin and app.runtime is None
    admin.health.assert_called_once()
    admin.summary.assert_called_once()
    factory.assert_not_called()
    assert "started elsewhere" in app.status.config.call_args.kwargs["text"]


@pytest.mark.parametrize("shutdown", [False, True])
def test_gui_attached_close_vs_explicit_stop(shutdown):
    app = gui.ControllerApp.__new__(gui.ControllerApp)
    app.admin = admin = Mock()
    app.local_agent = app.runtime = None
    app._stop_all(shutdown_attached=shutdown)
    assert admin.shutdown.call_count == int(shutdown)
    admin.close.assert_called_once()
