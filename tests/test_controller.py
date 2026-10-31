"""Controller runtime safety: refuses unsafe bind addresses, creates its own certificate and admin token."""
import pytest

from network_connector.config import ConfigError, ControllerConfig
from network_connector.controller import ControllerRuntime
from network_connector.security import fingerprint_pem_file


def test_controller_starts_without_console_streams(tmp_path, monkeypatch):
    import socket
    import sys

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    rt = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1", port=port), tmp_path)
    try:
        with monkeypatch.context() as patch:
            patch.setattr(sys, "stdout", None)
            patch.setattr(sys, "stderr", None)
            rt.start()
            assert rt._server.started
    finally:
        rt.stop()
        rt.store.close()


@pytest.mark.parametrize("host", ["0.0.0.0", "8.8.8.8"])
def test_runtime_refuses_wildcard_and_public_bind(tmp_path, host):
    with pytest.raises(ConfigError):
        ControllerRuntime(ControllerConfig(bind_host=host), tmp_path)


def test_runtime_creates_secrets_outside_repo_and_reuses_them(tmp_path):
    rt = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1"), tmp_path)
    for name in ("server.crt", "server.key", "admin.token", "controller.json", "state.db"):
        assert (tmp_path / name).is_file()
    token, fp = rt.admin_token, rt.fingerprint
    rt.store.close()
    again = ControllerRuntime(ControllerConfig(bind_host="127.0.0.1"), tmp_path)
    assert again.admin_token == token and again.fingerprint == fp == fingerprint_pem_file(tmp_path / "server.crt")
    again.store.close()
    assert len(token) >= 40
