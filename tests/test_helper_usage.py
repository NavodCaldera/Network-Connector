from datetime import date

from network_connector.helper_usage import HelperUsage
from network_connector.config import WorkerConfig
from network_connector.worker import WorkerAgent
from unittest.mock import Mock


def test_cpu_time_is_separate_by_requester_and_day(tmp_path):
    usage = HelperUsage(tmp_path / "usage.db")
    first = date(2026, 10, 28)
    next_day = date(2026, 10, 29)
    usage.add("requester-a", 40, first)
    usage.add("requester-a", 25, first)
    usage.add("requester-b", 10, first)
    usage.add("requester-a", 5, next_day)
    assert HelperUsage(tmp_path / "usage.db").seconds("requester-a", first) == 65
    assert usage.seconds("requester-b", first) == 10
    assert usage.seconds("requester-a", next_day) == 5


def test_worker_stops_claiming_at_daily_limit(tmp_path):
    cfg = WorkerConfig(ac_only=False, daily_cpu_min=1, requester_device_id="requester-a")
    agent = WorkerAgent(cfg, tmp_path)
    agent.state = "online"
    agent._event = Mock()
    agent.usage.add("requester-a", 60)
    assert agent._can_claim() is False
    agent._event.assert_called_with("daily CPU limit reached")


def test_requester_reputation_counts_killed_jobs(tmp_path):
    from network_connector.helper_usage import OUTCOME_KIND, HelperUsage

    usage = HelperUsage(tmp_path / "u.db")
    for outcome in ("memory", "timeout", "stdout_cap", "trap"):
        kind = OUTCOME_KIND.get(outcome)
        if kind:
            usage.note("r1", kind)
    usage.note("r1", "made-up")
    usage.note("", "limit")
    assert usage.reputation("r1") == {"limit": 2, "oversized": 1}
    assert usage.reputation("r2") == {}
