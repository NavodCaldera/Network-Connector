"""Controller discovery and desktop controls use pinned identities and background calls."""
from types import SimpleNamespace
import tkinter as tk
from tkinter import ttk

import pytest

from network_connector import cli, link_gui, wifidirect
from network_connector.config import WorkerConfig
from network_connector.link_gui import ControllerDirectControls, discover_controller


def test_paired_discovery_refuses_impostor_before_matching_pinned_cert(monkeypatch, tmp_path):
    cfg = WorkerConfig(controller_host="127.0.0.2", token="token", controller_fingerprint="wrong-config", known_hosts=["127.0.0.3"])
    calls = []
    monkeypatch.setattr(link_gui, "fingerprint_pem_file", lambda _p: "pinned")
    monkeypatch.setattr(wifidirect, "candidate_controllers", lambda: [])
    def probe(host, port):
        calls.append((host, port))
        return "impostor" if host.endswith("2") else "pinned"
    monkeypatch.setattr(wifidirect, "probe", probe)
    assert discover_controller(cfg, tmp_path) == {"host": "127.0.0.3", "fingerprint": "pinned"}
    assert len(calls) == 2
    monkeypatch.setattr(wifidirect, "probe", lambda *_args: "impostor")
    with pytest.raises(RuntimeError, match="pinned"):
        discover_controller(cfg, tmp_path)


def test_find_missing_pinned_file_fails_closed(monkeypatch, tmp_path):
    cfg = WorkerConfig(controller_host="127.0.0.2", token="token")
    monkeypatch.setattr(wifidirect, "probe", lambda *_args: pytest.fail("Must not probe without its saved pin"))
    with pytest.raises(FileNotFoundError):
        discover_controller(cfg, tmp_path)


def test_cli_direct_commands_and_worker_join(monkeypatch, capsys):
    calls = []
    admin = SimpleNamespace(direct_status=lambda: {"ssid": "DIRECT-NC-test", "passphrase": "secret"},
                            direct_start=lambda *args: calls.append(args) or {"running": True},
                            direct_stop=lambda: {"running": False})
    monkeypatch.setattr(cli, "_admin", lambda: admin)
    monkeypatch.setattr(cli, "load_dotenv", lambda: None)
    assert cli.main(["wifidirect", "status"]) == 0
    assert "secret" not in capsys.readouterr().out
    assert cli.main(["wifidirect", "start", "--ssid", "DIRECT-NC-test", "--no-listen"]) == 0
    assert calls == [("DIRECT-NC-test", None, False)]
    assert cli.main(["wifidirect", "stop"]) == 0
    parser = cli.build_parser()
    assert parser.parse_args(["worker", "join", "--ssid", "DIRECT-NC-test"]).fn is cli.cmd_worker_join
    assert parser.parse_args(["worker", "find"]).fn is cli.cmd_worker_find


def test_direct_gui_renders_mode_listener_and_fingerprint(monkeypatch):
    root = tk.Tk()
    root.withdraw()
    try:
        obj = ControllerDirectControls()
        obj.root = root
        obj.admin = None
        obj.cfg = SimpleNamespace(port=8765)
        obj.bg = SimpleNamespace(run=lambda work, done, error=None: done(work()))
        monkeypatch.setattr(link_gui, "firewall_status", lambda _host: "narrow rule present")
        obj._build_direct(ttk.Notebook(root))
        obj.direct_pair_hint = ttk.Label(root)
        obj._direct_render({"mode": "B", "ip": "192.168.137.1", "interface": "direct", "running": True,
                            "listeners": ["192.168.137.1"], "fingerprint": "ab12", "peers": ["192.168.137.2"]})
        assert obj.direct_listen.get()
        assert "192.168.137.1:8765" in obj.direct_pair_hint.cget("text")
        assert "ab12" in obj.direct_pair_hint.cget("text")
        assert "Mode: B" in obj.direct_status.cget("text")
        obj.direct_ssid.set("DIRECT-NC-old")
        obj.direct_password.set("generated-old")
        obj._direct_render({"running": False, "mode": "none"})
        assert obj.direct_ssid.get() == obj.direct_password.get() == ""
        obj.direct_reuse.set(True)
        obj._direct_render({"running": True, "ssid": "DIRECT-NC-pinned", "passphrase": "pinned-password"})
        obj._direct_render({"running": False})
        assert obj.direct_password.get() == "pinned-password"
    finally:
        root.destroy()


def test_both_windows_construct_with_connection_and_benchmark_controls(monkeypatch, tmp_path):
    from network_connector import gui, worker_gui

    monkeypatch.setattr(gui, "controller_dir", lambda: tmp_path / "controller")
    monkeypatch.setattr(gui, "list_private_ips", lambda labelled=False: ["192.168.1.4 — Ethernet"])
    monkeypatch.setattr(worker_gui, "worker_dir", lambda _name: tmp_path / "worker")
    monkeypatch.setattr(worker_gui, "hardware_info", lambda: {"hostname": "test", "cpu_name": "test CPU", "cores_logical": 4, "ram_total_mb": 4096})
    for app_type in (gui.ControllerApp, worker_gui.WorkerApp):
        root = tk.Tk()
        root.withdraw()
        try:
            app = app_type(root)
            root.update_idletasks()
            if app_type is gui.ControllerApp:
                assert app.direct_status.winfo_exists()
                assert app.bench_table.winfo_exists()
            else:
                assert app.link_status.winfo_exists()
                assert app.performance.winfo_exists()
        finally:
            root.destroy()


def test_controller_observed_link_is_kept_with_job_and_benchmark_worker(tmp_path):
    from network_connector.store import Store

    store = Store(tmp_path / "state.db")
    try:
        wid = store.add_worker("peer", "hash", {})
        store.touch_worker(wid, None, {"link": "wifi-direct"}, "0.2.0")
        store.create_batch("number_batch", None, None, [("number_batch", {"count": 1})], 60, 1, None)
        job = store.claim({"id": wid, "name": "peer"}, 1, 60)[0]
        assert store.get_job(job["id"])["link"] == "wifi-direct"
        assert store.list_workers()[0]["link"] == "wifi-direct"
    finally:
        store.close()


@pytest.mark.parametrize("surface", ["gui", "cli"])
def test_changing_primary_address_keeps_only_explicit_extra_listeners(surface, monkeypatch, tmp_path):
    from network_connector import controller, gui
    from network_connector.config import ControllerConfig, load_config, save_config

    path = tmp_path / "controller.json"
    save_config(ControllerConfig(bind_host="127.0.0.2", bind_hosts=["127.0.0.2", "127.0.0.3", "127.0.0.1"]), path)
    if surface == "gui":
        app = gui.ControllerApp.__new__(gui.ControllerApp)
        app.cfg_path = path
        app.bind_var = SimpleNamespace(get=lambda: "127.0.0.4 — test adapter")
        app.port_var = SimpleNamespace(get=lambda: 8765)
        app.save_settings(quiet=True)
        result = load_config(ControllerConfig, path)
    else:
        captured = []
        def runtime(cfg):
            captured.append(cfg)
            return SimpleNamespace(cfg=cfg, start=lambda: None, wait=lambda: None, fingerprint="test")
        monkeypatch.setattr(controller, "ControllerRuntime", runtime)
        monkeypatch.setattr(cli, "controller_dir", lambda: tmp_path)
        monkeypatch.setattr(cli, "setup_logging", lambda _name: None)
        cli.cmd_controller_start(SimpleNamespace(host="127.0.0.4", port=None))
        result = captured[0]
    assert result.bind_host == "127.0.0.4"
    assert result.bind_hosts == ["127.0.0.3", "127.0.0.1"]


def test_forget_pairing_clears_discovery_identity(monkeypatch, tmp_path):
    from network_connector import worker_gui
    from network_connector.config import load_config, save_config

    path = tmp_path / "worker.json"
    save_config(WorkerConfig(token="secret", worker_id="old", controller_fingerprint="old-pin", known_hosts=["127.0.0.2"]), path)
    (tmp_path / "controller.crt").write_text("old certificate", encoding="utf-8")
    app = worker_gui.WorkerApp.__new__(worker_gui.WorkerApp)
    app.agent, app.dir, app.cfg_path = None, tmp_path, path
    app._refresh_paired = lambda: None
    monkeypatch.setattr(worker_gui.messagebox, "askyesno", lambda *_args: True)
    app.forget()
    cfg = load_config(WorkerConfig, path)
    assert cfg.token == cfg.worker_id == cfg.controller_fingerprint == ""
    assert cfg.known_hosts == []
    assert not (tmp_path / "controller.crt").exists()
