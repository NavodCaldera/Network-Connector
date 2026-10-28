"""Start at login: one HKCU Run value for this user, removable, no admin."""
import types

from network_connector import autostart


class FakeKey:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def _fake_winreg(store):
    reg = types.SimpleNamespace(HKEY_CURRENT_USER="HKCU", REG_SZ=1)
    reg.CreateKey = lambda root, path: (store.setdefault(path, {}), FakeKey())[1]
    def open_key(root, path):
        if path not in store:
            raise OSError("missing")
        return FakeKey()
    reg.OpenKey = open_key
    reg.SetValueEx = lambda key, name, _r, _t, value: store[autostart.RUN_KEY].__setitem__(name, value)
    def query(key, name):
        if name not in store[autostart.RUN_KEY]:
            raise OSError("missing")
        return store[autostart.RUN_KEY][name], 1
    reg.QueryValueEx = query
    def delete(key, name):
        if name not in store[autostart.RUN_KEY]:
            raise OSError("missing")
        del store[autostart.RUN_KEY][name]
    reg.DeleteValue = delete
    return reg


def test_enable_and_disable(monkeypatch):
    store = {}
    monkeypatch.setattr(autostart.sys, "platform", "win32")
    monkeypatch.setattr(autostart, "_winreg", lambda: _fake_winreg(store))
    assert not autostart.enabled()
    autostart.set_enabled(True)
    value = store[autostart.RUN_KEY][autostart.VALUE]
    assert value.endswith(' -m network_connector.app --background') and value.startswith('"')
    assert autostart.enabled()
    autostart.set_enabled(False)
    assert not autostart.enabled()
    autostart.set_enabled(False)  # already off: no error


def test_not_supported_off_windows(monkeypatch):
    monkeypatch.setattr(autostart.sys, "platform", "linux")
    assert autostart.set_enabled(True) is False
    assert autostart.enabled() is False
