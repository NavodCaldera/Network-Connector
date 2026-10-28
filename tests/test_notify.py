"""Owner messages: strict toast arguments, the Ask-me timeout, and no secrets in the text (PLAN §16, invariant 11)."""
from unittest.mock import Mock

from network_connector import notify
from network_connector.notify import ConsentRequests, Notifier, ask_text, parse_argument, using_text

DEV = "ab" * 32


def test_only_accept_decline_and_stop_arguments_are_understood():
    assert parse_argument(f"accept:{DEV}") == ("accept", DEV)
    assert parse_argument(f"decline:{DEV}") == ("decline", DEV)
    assert parse_argument(f"stop:{DEV}") == ("stop", DEV)
    for bad in (f"run:{DEV}", f"accept:{DEV[:-1]}", f"accept:{DEV.upper()}", f"accept:{DEV} ", "accept", None, 5,
                f"accept:{DEV}\naccept:{DEV}", "x" * 500):
        assert parse_argument(bad) is None


def test_unanswered_question_expires_to_decline_after_60_seconds():
    now = [100.0]
    requests = ConsentRequests(clock=lambda: now[0])
    assert requests.timeout_s == 60
    assert requests.add(DEV, "Navod's i7", "ABAB-ABAB", 4)
    assert not requests.add(DEV, "again", "ABAB-ABAB", 4)  # no repeated pop-ups for one device
    now[0] += 59.9
    assert requests.expire() == []
    now[0] += 0.2
    assert [r.device_id for r in requests.expire()] == [DEV]
    assert requests.answer("accept", DEV) is None  # a late Accept does nothing


def test_answer_closes_the_question_once():
    requests = ConsentRequests()
    requests.add(DEV, "i7", "ABAB-ABAB", 2)
    assert requests.answer("stop", DEV) is None  # not an answer to a question
    assert requests.answer("accept", DEV).name == "i7"
    assert requests.answer("accept", DEV) is None
    assert requests.pending() == []


def test_message_text_holds_only_name_short_id_and_threads():
    text = using_text("Navod's i7\x07", "AB12-CD34", 4)
    assert text == "Navod's i7 (AB12-CD34) is using 4 of your CPU threads."
    assert "sandbox" in ask_text("i7", "AB12-CD34", 2) and "Decline" in ask_text("i7", "AB12-CD34", 2)


def test_toast_xml_escapes_requester_supplied_names():
    xml = notify._toast_xml(using_text('<x a="1">&', "AB12-CD34", 1), [("Stop", f"stop:{DEV}")], reminder=False)
    assert "<x" not in xml and "&amp;" in xml and f'arguments="stop:{DEV}"' in xml


def test_without_toasts_messages_fall_back_to_the_tray_then_the_window(monkeypatch):
    monkeypatch.setattr(notify.sys, "platform", "linux")
    window = Mock()
    tray = Mock(can_notify=True)
    n = Notifier(Mock(), tray=tray, window=window)
    assert n.channel == "window"
    assert n.info("hello", DEV) == "tray"
    tray.notify.assert_called_once_with("hello")
    tray.can_notify = False
    assert n.info("again") == "window"
    window.assert_called_with("info", "again", "")
    request = notify.ConsentRequest(DEV, "i7", "ABAB-ABAB", 2, 0)
    assert n.ask(request) == "window"
    assert window.call_args[0][0] == "ask" and window.call_args[0][2] == DEV
