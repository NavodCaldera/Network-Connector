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
