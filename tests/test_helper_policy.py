from datetime import datetime
from types import SimpleNamespace

from network_connector.config import WorkerConfig
from network_connector.helper_policy import can_help


def test_blocked_requester_is_refused():
    cfg = WorkerConfig(ac_only=False, requester_device_id="blocked", blocked_requesters=["blocked"])
    assert can_help(cfg)[0] is False


def test_ac_and_idle_gates(monkeypatch):
    cfg = WorkerConfig(ac_only=True, idle_min=5)
    monkeypatch.setattr("network_connector.helper_policy.psutil.sensors_battery",
                        lambda: SimpleNamespace(power_plugged=False))
    assert can_help(cfg)[1] == "waiting for AC power"
    monkeypatch.setattr("network_connector.helper_policy.psutil.sensors_battery",
                        lambda: SimpleNamespace(power_plugged=True))
    monkeypatch.setattr("network_connector.helper_policy.input_idle_seconds", lambda: 299)
    assert can_help(cfg)[1] == "waiting until this computer is idle"
    monkeypatch.setattr("network_connector.helper_policy.input_idle_seconds", lambda: 300)
    assert can_help(cfg)[0] is True


def test_hours_include_overnight_window():
    cfg = WorkerConfig(ac_only=False, allowed_hours="22:00-06:00")
    assert can_help(cfg, datetime(2026, 1, 1, 23, 0))[0]
    assert can_help(cfg, datetime(2026, 1, 2, 5, 59))[0]
    assert not can_help(cfg, datetime(2026, 1, 2, 6, 0))[0]
    cfg.allowed_hours = "bad"
    assert not can_help(cfg)[0]
