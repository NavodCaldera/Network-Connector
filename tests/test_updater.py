import subprocess

import pytest

from network_connector import updater


@pytest.mark.parametrize("failure", ["exit", "timeout"])
def test_fetch_offline_has_bounded_timeout_and_clear_message(monkeypatch, tmp_path, failure):
    calls = []
    def run(args, cwd, timeout):
        calls.append((args, timeout))
        if failure == "timeout":
            raise updater.UpdateError("command timed out")
        return subprocess.CompletedProcess(args, 1, "", "network unreachable")
    monkeypatch.setattr(updater, "_run", run)
    with pytest.raises(updater.UpdateError, match="No internet connection; connect to a normal network to update"):
        updater.latest_tag(tmp_path)
    assert calls[0][1] == 30
    assert calls[0][0][0:2] == ["git", "fetch"]


def test_nonfetch_errors_keep_specific_cause(monkeypatch, tmp_path):
    monkeypatch.setattr(updater, "_run", lambda args, cwd, timeout: subprocess.CompletedProcess(args, 1, "", "repository missing"))
    with pytest.raises(updater.UpdateError, match="repository missing"):
        updater._git(tmp_path, "status", "--porcelain")
