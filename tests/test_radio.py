import pytest

from network_connector import radio


def test_off_windows_the_radio_is_unknown_and_never_switched(monkeypatch):
    monkeypatch.setattr(radio.sys, "platform", "linux")
    assert radio.wifi_state() == "unknown"
    with pytest.raises(radio.RadioError):
        radio.turn_wifi_on()


def test_module_has_no_way_to_turn_a_radio_off():
    import inspect

    source = inspect.getsource(radio)
    assert "RadioState.OFF" not in source and "turn_wifi_off" not in source
