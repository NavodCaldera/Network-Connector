import socket
import threading

from network_connector import cli, hello


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test_hello_both_ways():
    port, heard = _free_port(), []
    ready = threading.Event()

    def note(msg):
        heard.append(msg)
        ready.set()

    t = threading.Thread(target=hello.serve, args=("127.0.0.1", port, True, note), daemon=True)
    t.start()
    ready.wait(5)
    assert hello.send("127.0.0.1", port) == hello.greeting()
    t.join(5)
    assert any(m.endswith(f"says: {hello.greeting()}") for m in heard)


def test_cli_send_reports_unreachable(capsys):
    assert cli.main(["send", "127.0.0.1", "--port", str(_free_port())]) == 1
    assert "Error" in capsys.readouterr().out


def test_join_without_arguments_finds_group_and_uses_built_in_password(monkeypatch):
    from network_connector import wifidirect

    ssid = wifidirect.default_ssid()
    joined = {}
    monkeypatch.setattr(cli, "scan_direct_groups", lambda: [ssid])
    monkeypatch.setattr(cli, "join", lambda s, p: joined.update(ssid=s, passphrase=p) or
                        wifidirect.InterfaceInfo("w", "d", "192.168.137.5", "192.168.137.0/24"))
    monkeypatch.setattr(cli, "candidate_controllers", lambda iface: [])
    assert cli.main(["join"]) == 1  # nobody answers, but the join itself used the built-in password
    assert joined == {"ssid": ssid, "passphrase": wifidirect.derived_passphrase(ssid)}
    wifidirect.validate_credentials(ssid, joined["passphrase"])


def test_firewall_problems_parse_counts(monkeypatch):
    from types import SimpleNamespace

    from network_connector import firewall

    monkeypatch.setattr(firewall.sys, "platform", "win32")
    monkeypatch.setattr(firewall, "_ps", lambda script, timeout=30: SimpleNamespace(stdout="2 0\r\n"))
    assert len(firewall.problems()) == 2
    monkeypatch.setattr(firewall, "_ps", lambda script, timeout=30: SimpleNamespace(stdout="0 1\r\n"))
    assert firewall.problems() == []
    monkeypatch.setattr(firewall, "_ps", lambda script, timeout=30: SimpleNamespace(stdout="garbage"))
    assert firewall.problems() == []  # unknown state never nags


def test_gui_send_shows_both_directions():
    import pytest
    tk = pytest.importorskip("tkinter")
    from network_connector import gui

    try:
        root = tk.Tk()
    except tk.TclError:
        pytest.skip("no display")
    root.withdraw()
    port = _free_port()
    threading.Thread(target=hello.serve, args=("127.0.0.1", port, True, lambda m: None), daemon=True).start()
    try:
        app = gui.App(root)
        app.peer = "127.0.0.1"
        orig = hello.send
        hello.send = lambda host, p=hello.PORT, timeout=10: orig(host, port, timeout)
        try:
            for _ in range(20):
                if app.send():
                    break
        finally:
            hello.send = orig
        app.pump()
        text = app.log.get("1.0", "end")
        assert f"-> {hello.greeting()}" in text and f"<- 127.0.0.1 replied: {hello.greeting()}" in text
    finally:
        root.destroy()
