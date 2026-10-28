"""The owner Stop must prevent another claim and kill running work within 1 s (PLAN §16.2, AGENT.md invariant 9)."""
import threading
import time
from types import SimpleNamespace
from unittest.mock import Mock

import psutil

from network_connector.worker import WorkerAgent

from conftest import wait_for


def test_stop_helping_pauses_before_killing_sandboxes(monkeypatch):
    agent = WorkerAgent.__new__(WorkerAgent)
    agent.cfg = SimpleNamespace(group_mode="open")
    agent.local_paused = False
    agent._immediate_stop = False
    agent._leave = False
    agent.stop_event = threading.Event()
    agent._event = Mock()
    killed = Mock()
    monkeypatch.setattr("network_connector.worker.sandbox.stop_all", killed)

    agent.stop_helping()

    assert agent.local_paused
    assert agent._immediate_stop
    assert agent._leave
    assert agent.stop_event.is_set()
    killed.assert_called_once_with()


def _running_pid(agent):
    jobs = list(agent.running.values())
    return jobs[0].proc.pid if jobs and jobs[0].proc else None


def test_stop_kills_a_running_job_within_one_second_and_hands_it_back(cluster):
    agent = cluster.add_worker("stopme", max_concurrent_jobs=1, max_cpu_threads=1, ac_only=False)
    batch = cluster.admin.submit("cpu_burn", {"seconds": 60}, max_attempts=2)["batch_id"]
    pid = wait_for(lambda: _running_pid(agent), timeout=30)
    started = time.monotonic()
    agent.stop_helping()
    wait_for(lambda: not psutil.pid_exists(pid) or psutil.Process(pid).status() == psutil.STATUS_ZOMBIE,
             timeout=1.0, interval=0.02)
    assert time.monotonic() - started < 1.0
    agent.stop()
    job = cluster.admin.jobs(batch_id=batch)[0]
    assert job["status"] == "queued"  # released: it did not use up an attempt
    assert job["attempt"] == 0


def test_limit_reached_while_running_stops_the_job_when_the_owner_chose_immediately(cluster):
    agent = cluster.add_worker("limits", max_concurrent_jobs=1, max_cpu_threads=1, ac_only=False, limit_stop_now=True)
    batch = cluster.admin.submit("cpu_burn", {"seconds": 60}, max_attempts=2)["batch_id"]
    pid = wait_for(lambda: _running_pid(agent), timeout=30)
    agent.cfg.daily_cpu_min = 1
    agent.usage.add(agent._requester_key(), 61)  # the day's limit is now used up
    wait_for(lambda: not agent.running, timeout=5, interval=0.05)
    assert not psutil.pid_exists(pid) or psutil.Process(pid).status() == psutil.STATUS_ZOMBIE
    assert agent.snapshot()["policy_reason"] == "daily CPU limit reached"
    wait_for(lambda: cluster.admin.jobs(batch_id=batch)[0]["status"] == "queued", timeout=5)


def test_limit_reached_while_running_lets_the_job_finish_by_default(cluster):
    agent = cluster.add_worker("finish", max_concurrent_jobs=1, max_cpu_threads=1, ac_only=False)
    batch = cluster.admin.submit("cpu_burn", {"seconds": 3})["batch_id"]
    wait_for(lambda: _running_pid(agent), timeout=30)
    agent.cfg.allowed_hours = "00:00-00:00"  # never allowed
    wait_for(lambda: cluster.admin.jobs(batch_id=batch)[0]["status"] == "succeeded", timeout=30)
    assert agent.snapshot()["policy_reason"] == "outside allowed helping hours"
