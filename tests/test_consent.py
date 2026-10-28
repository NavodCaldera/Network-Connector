"""Helper-side consent: Ask me first waits for Accept, auto-accept tells the owner, and Stop from a message works."""
from types import SimpleNamespace
from unittest.mock import Mock

from network_connector import link_gui
from network_connector.config import WorkerConfig
from network_connector.link_gui import WorkerNearbyControls
from network_connector.notify import ConsentRequests

DEV = "cd" * 32


def _helper(mode: str, monkeypatch):
    monkeypatch.setattr(link_gui, "sandbox_ready", lambda: True)
    obj = WorkerNearbyControls()
    obj.cfg = WorkerConfig(join_mode=mode, max_cpu_threads=3, max_concurrent_jobs=4)
    obj.agent = None
    obj.peers = SimpleNamespace(is_blocked=lambda d: False, is_trusted=lambda d: False)
    obj.consent = ConsentRequests()
    obj._consent_windows = {}
    obj.notifier = Mock()
    obj._join_entry = Mock()
    obj._write = Mock()
    obj.stop = Mock()
    entry = SimpleNamespace(device_id=DEV, name="Navod's i7", short_id="CDCD-CDCD", open=True)
    obj.nearby = SimpleNamespace(get=lambda d: entry if d == DEV else None)
    return obj, entry


def test_ask_me_first_waits_for_accept(monkeypatch):
    obj, entry = _helper("ask", monkeypatch)
    obj._nearby_new(entry)
    obj._join_entry.assert_not_called()
    request = obj.notifier.ask.call_args[0][0]
    assert (request.device_id, request.threads) == (DEV, 3)
    obj._nearby_new(entry)  # seen again: still one question
    assert obj.notifier.ask.call_count == 1
    obj._notify_action("accept", DEV)
    obj._join_entry.assert_called_once_with(entry, ask=False, announce=True)
    obj._notify_action("accept", DEV)  # a second click joins nothing more
    assert obj._join_entry.call_count == 1


def test_decline_and_blocked_requesters_never_join(monkeypatch):
    obj, entry = _helper("ask", monkeypatch)
    obj._nearby_new(entry)
    obj._notify_action("decline", DEV)
    obj._join_entry.assert_not_called()
    obj.peers.is_blocked = lambda d: True
    obj._nearby_new(entry)
    assert obj.notifier.ask.call_count == 1


def test_auto_accept_joins_and_needs_the_sandbox(monkeypatch):
    obj, entry = _helper("auto", monkeypatch)
    obj._nearby_new(entry)
    obj._join_entry.assert_called_once_with(entry, ask=False, announce=True)
    monkeypatch.setattr(link_gui, "sandbox_ready", lambda: False)
    obj._join_entry.reset_mock()
    obj._nearby_new(entry)
    obj._join_entry.assert_not_called()


def test_stop_button_stops_only_the_matching_requester(monkeypatch):
    obj, _entry = _helper("auto", monkeypatch)
    obj.agent = SimpleNamespace(cfg=WorkerConfig(requester_device_id=DEV, controller_fingerprint=DEV))
    obj._notify_action("stop", "ee" * 32)
    obj.stop.assert_not_called()
    obj._notify_action("stop", DEV)
    obj.stop.assert_called_once_with()
