import tkinter as tk
from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from network_connector import app as app_module
from network_connector.app import NetworkConnectorApp


@pytest.fixture
def quiet(monkeypatch):
    monkeypatch.setattr("network_connector.identity.current", lambda: SimpleNamespace(name="test", short_id="1234-5678", device_id="0" * 64))
    monkeypatch.setattr("network_connector.identity.PeerBook", lambda: SimpleNamespace(blocked=lambda: []))
    monkeypatch.setattr("network_connector.discovery.Listener.start", lambda self: None)
    monkeypatch.setattr("network_connector.discovery.Listener.stop", lambda self: None)
    monkeypatch.setattr(app_module.autostart, "set_enabled", Mock())
    root = tk.Tk()
    root.withdraw()
    yield root
    root.destroy()


def test_both_roles_render_in_one_window(quiet):
    app = NetworkConnectorApp(quiet, tray=False)
    app.set_role("both")
    quiet.update_idletasks()
    assert app.controller is not None
    assert app.worker is not None
    assert app.controller.root.winfo_toplevel() is quiet
    assert app.worker.root.winfo_toplevel() is quiet
    app_module.autostart.set_enabled.assert_called_once_with(True)  # first choice writes the login entry once
    app.set_role("both")
    assert app_module.autostart.set_enabled.call_count == 1
    app.worker._nearby_stop()


def test_tray_actions_reach_the_helper_and_close_hides_to_tray(quiet):
    app = NetworkConnectorApp(quiet, tray=False)
    app.set_role("give")
    app.tray = Mock()
    app.worker.stop = Mock()
    app.worker.toggle_pause = Mock()
    app.worker.agent = SimpleNamespace(local_paused=False, running={}, state="online")
    app.tray_action("stop")
    app.worker.stop.assert_called_once_with()
    app.tray_action("pause")
    app.worker.toggle_pause.assert_called_once_with()
    assert app.tray_status() == "Helping: online · 0 running"
    quiet.deiconify()
    app.on_close()
    assert quiet.state() == "withdrawn"  # still helping in the background
    app.tray_action("open")
    assert quiet.state() == "normal"
    app.worker.agent = None
    app.worker._nearby_stop()
