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


def test_helper_policy_round_trips_through_worker_json(tmp_path):
    import pytest

    from network_connector.config import ConfigError, load_config, save_config
    from network_connector.helper_policy import HelperPolicy

    path = tmp_path / "worker.json"
    cfg = WorkerConfig()
    policy = HelperPolicy(accept="ask", threads=3, jobs=2, mem_mb=2048, ac_only=False, idle_min=5,
                          hours="08:00-18:00", daily_cpu_min=30, blocked=["c" * 64, "c" * 64], stop_now=True)
    save_config(policy.apply(cfg), path)
    loaded = HelperPolicy.from_config(load_config(WorkerConfig, path))
    assert loaded == HelperPolicy(accept="ask", threads=3, jobs=2, mem_mb=2048, ac_only=False, idle_min=5,
                                  hours="08:00-18:00", daily_cpu_min=30, blocked=["c" * 64], stop_now=True)
    for bad in (dict(accept="always"), dict(hours="8-18"), dict(threads=0), dict(blocked=["short"]), dict(daily_cpu_min=True)):
        with pytest.raises(ConfigError):
            HelperPolicy(**bad).validate()


def test_limit_reason_counts_running_time_and_blocks_first():
    from network_connector.helper_policy import limit_reason

    cfg = WorkerConfig(ac_only=False, daily_cpu_min=2)
    assert limit_reason(cfg, 119) == ""
    assert limit_reason(cfg, 120) == "daily CPU limit reached"
    assert limit_reason(cfg, 0, blocked=True) == "requester blocked by this helper"
    cfg.daily_cpu_min = 0
    assert limit_reason(cfg, 10**9) == ""  # 0 = unlimited


def test_cpu_rate_cap_is_validated_and_shared_between_slots():
    import pytest

    from network_connector.winlimits import cpu_rate_value
    from network_connector.worker import WorkerAgent

    assert cpu_rate_value(1) == 100 and cpu_rate_value(100) == 10000
    for bad in (0, 101, True, 2.5):
        with pytest.raises(ValueError):
            cpu_rate_value(bad)
    agent = WorkerAgent.__new__(WorkerAgent)
    agent.cfg = WorkerConfig(cpu_rate_percent=50, max_cpu_threads=4, max_concurrent_jobs=4)
    assert agent._cpu_rate_per_job() == 12
    agent.cfg.cpu_rate_percent = 0
    assert agent._cpu_rate_per_job() == 0
