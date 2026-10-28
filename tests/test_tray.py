"""Tray menu clicks are queued for tk; nothing runs on pystray's thread (Phase 6 spike)."""
import sys
import types
from unittest.mock import Mock

from network_connector.tray import Tray


def _fake_pystray(monkeypatch):
    module = types.ModuleType("pystray")
    module.MenuItem = lambda text, action, **kw: Mock(text=text, action=action, kw=kw)
    module.Menu = lambda *items: list(items)
    icon = Mock(HAS_NOTIFICATION=True)
    module.Icon = Mock(return_value=icon)
    monkeypatch.setitem(sys.modules, "pystray", module)
    return module, icon


def test_menu_actions_are_queued_and_run_on_poll(monkeypatch):
    module, icon = _fake_pystray(monkeypatch)
    tray = Tray(lambda: "Helping: online", lambda: False)
    assert tray.start()
    icon.run_detached.assert_called_once_with()
    menu = module.Icon.call_args[0][3]
    assert [item.text for item in menu[1:]] == ["Pause", "Stop helping", "Open", "Quit"]
    assert menu[0].text(None) == "Helping: online"
    handled = []
    for item in menu[1:]:
        item.action(icon, item)  # what pystray's thread does
    assert handled == []  # nothing ran yet
    tray.actions.put("rm -rf")  # unknown actions are dropped
    tray.poll(handled.append)
    assert handled == ["pause", "stop", "open", "quit"]
    assert tray.can_notify
    tray.stop()
    icon.stop.assert_called_once_with()
    assert not tray.running


def test_no_pystray_means_no_tray(monkeypatch):
    monkeypatch.setitem(sys.modules, "pystray", None)
    tray = Tray(lambda: "", lambda: False)
    assert tray.start() is False
    assert not tray.running and not tray.can_notify
