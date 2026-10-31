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
