import tkinter as tk
from types import SimpleNamespace

from network_connector.app import NetworkConnectorApp


def test_both_roles_render_in_one_window(monkeypatch):
    monkeypatch.setattr("network_connector.identity.current", lambda: SimpleNamespace(name="test", short_id="1234-5678", device_id="0" * 64))
    monkeypatch.setattr("network_connector.identity.PeerBook", lambda: SimpleNamespace())
    monkeypatch.setattr("network_connector.discovery.Listener.start", lambda self: None)
    monkeypatch.setattr("network_connector.discovery.Listener.stop", lambda self: None)
    root = tk.Tk()
    root.withdraw()
    try:
        app = NetworkConnectorApp(root)
        app.set_role("both")
        root.update_idletasks()
        assert app.controller is not None
        assert app.worker is not None
        assert app.controller.root.winfo_toplevel() is root
        assert app.worker.root.winfo_toplevel() is root
        app.worker._nearby_stop()
    finally:
        root.destroy()
