"""The owner Stop must prevent another claim and kill running sandboxes promptly."""
import threading
from types import SimpleNamespace
from unittest.mock import Mock

from network_connector.worker import WorkerAgent


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
