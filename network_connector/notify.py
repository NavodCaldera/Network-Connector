"""Messages to the helper's owner: Windows toasts, with the tray balloon and the app window as fallbacks.

Only names, short IDs and thread counts are ever shown (AGENT.md §1 invariant 11). Toast buttons carry
`accept:<device id>`, `decline:<device id>` or `stop:<device id>`; any other argument is ignored.
"""
from __future__ import annotations

import logging
import re
import sys
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable
from xml.sax.saxutils import escape, quoteattr

log = logging.getLogger("network_connector.notify")

AUMID = "NetworkConnector.App"
DISPLAY_NAME = "Network Connector"
_AUMID_KEY = rf"Software\Classes\AppUserModelId\{AUMID}"
_SETTINGS_KEY = rf"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\{AUMID}"
ASK_TIMEOUT_S = 60.0
ACTIONS = ("accept", "decline", "stop")
_ARGUMENT = re.compile(r"^(accept|decline|stop):([0-9a-f]{64})$")

Action = Callable[[str, str], None]  # (action, device_id); may be called from any thread


def parse_argument(argument: Any) -> tuple[str, str] | None:
    """('accept'|'decline'|'stop', device_id) for a valid toast argument, else None."""
    if not isinstance(argument, str) or len(argument) > 80:
        return None
    match = _ARGUMENT.fullmatch(argument)
    return (match[1], match[2]) if match else None


def _clean(text: str, limit: int = 120) -> str:
    return "".join(ch for ch in str(text) if ch.isprintable())[:limit]


def using_text(name: str, short_id: str, threads: int) -> str:
    return f"{_clean(name, 40)} ({_clean(short_id, 9)}) is using {int(threads)} of your CPU threads."


def ask_text(name: str, short_id: str, threads: int) -> str:
    return (f"{_clean(name, 40)} ({_clean(short_id, 9)}) asks to use up to {int(threads)} of your CPU threads. "
            f"Work runs in the sandbox. No answer in {int(ASK_TIMEOUT_S)} s means Decline.")


# ------------------------------------------------------------------ AUMID registration (per user, no admin)
def _winreg() -> Any:
    import winreg

    return winreg


def register_aumid(icon_path: str = "") -> bool:
    """Register the app's AppUserModelID under HKCU so Windows shows its toasts. Idempotent."""
    if sys.platform != "win32":
        return False
    reg = _winreg()
    key = reg.CreateKey(reg.HKEY_CURRENT_USER, _AUMID_KEY)
    try:
        reg.SetValueEx(key, "DisplayName", 0, reg.REG_SZ, DISPLAY_NAME)
        if icon_path:
            reg.SetValueEx(key, "IconUri", 0, reg.REG_SZ, icon_path)
    finally:
        reg.CloseKey(key)
    return True


def unregister_aumid() -> None:
    """Remove the AUMID and the notification settings Windows created for it (reset/uninstall)."""
    if sys.platform != "win32":
        return
    reg = _winreg()
    for path in (_AUMID_KEY, _SETTINGS_KEY):
        try:
            reg.DeleteKey(reg.HKEY_CURRENT_USER, path)
        except OSError:
            pass


# ------------------------------------------------------------------ Ask-me requests
@dataclass
class ConsentRequest:
    device_id: str
    name: str
    short_id: str
    threads: int
    deadline: float


@dataclass
class ConsentRequests:
    """Pending Ask-me-first questions. A question without an answer expires to Decline."""
    timeout_s: float = ASK_TIMEOUT_S
    clock: Callable[[], float] = time.monotonic
    _pending: dict[str, ConsentRequest] = field(default_factory=dict)
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def add(self, device_id: str, name: str, short_id: str, threads: int) -> bool:
        """False when this device already has an open question (no repeated pop-ups)."""
        with self._lock:
            if device_id in self._pending or len(self._pending) >= 8:
                return False
            self._pending[device_id] = ConsentRequest(device_id, name, short_id, threads, self.clock() + self.timeout_s)
            return True

    def answer(self, action: str, device_id: str) -> ConsentRequest | None:
        """The request if it was open; it is closed either way. Late answers return None (already declined)."""
        if action not in ("accept", "decline"):
            return None
        with self._lock:
            request = self._pending.pop(device_id, None)
        if request is None or self.clock() >= request.deadline:
            return None
        return request

    def expire(self) -> list[ConsentRequest]:
        now = self.clock()
        with self._lock:
            gone = [r for r in self._pending.values() if now >= r.deadline]
            for r in gone:
                del self._pending[r.device_id]
        return gone

    def pending(self) -> list[ConsentRequest]:
        with self._lock:
            return list(self._pending.values())


# ------------------------------------------------------------------ delivery
def _toast_xml(text: str, buttons: list[tuple[str, str]], reminder: bool) -> str:
    actions = "".join(f"<action content={quoteattr(label)} arguments={quoteattr(arg)} activationType=\"foreground\"/>"
                      for label, arg in buttons)
    scenario = ' scenario="reminder"' if reminder else ""
    return (f"<toast{scenario}><visual><binding template=\"ToastGeneric\"><text>{escape(DISPLAY_NAME)}</text>"
            f"<text>{escape(text)}</text></binding></visual>"
            + (f"<actions>{actions}</actions>" if actions else "") + "</toast>")


class Notifier:
    """Shows owner messages through the best channel that works: toast, then tray balloon, then the window.

    `on_action(action, device_id)` receives button clicks from any thread; callers hand them to tk.
    `window(kind, text, device_id)` is the in-app fallback (kind "info" or "ask").
    """

    def __init__(self, on_action: Action, tray: Any = None, window: Callable[[str, str, str], None] | None = None) -> None:
        self.on_action, self.tray, self.window = on_action, tray, window
        self._notifier: Any = None
        self._toasts: dict[str, Any] = {}  # keep toast objects alive so their events still fire
        self.channel = "window"
        self._init_toasts()

    def _init_toasts(self) -> None:
        if sys.platform != "win32":
            return
        try:
            from winrt.windows.ui.notifications import NotificationSetting, ToastNotificationManager

            register_aumid()
            notifier = ToastNotificationManager.create_toast_notifier_with_id(AUMID)
            try:
                enabled = notifier.setting == NotificationSetting.ENABLED
            except OSError:
                enabled = True  # a never-used AUMID reports "Element not found" until its first toast (Phase 6)
            if enabled:
                self._notifier, self.channel = notifier, "toast"
        except Exception as exc:  # noqa: BLE001 - missing winrt packages or a policy that blocks toasts
            log.info("toasts unavailable (%s); using the tray or the window", type(exc).__name__)

    def _fallback(self, kind: str, text: str, device_id: str) -> str:
        if kind == "info" and self.tray is not None and getattr(self.tray, "can_notify", False):
            try:
                self.tray.notify(text)
                return "tray"
            except Exception:  # noqa: BLE001
                pass
        if self.window:
            self.window(kind, text, device_id)
        return "window"

    def _show_toast(self, tag: str, text: str, buttons: list[tuple[str, str]], reminder: bool) -> bool:
        if self._notifier is None:
            return False
        try:
            from winrt.windows.data.xml.dom import XmlDocument
            from winrt.windows.ui.notifications import ToastActivatedEventArgs, ToastNotification

            doc = XmlDocument()
            doc.load_xml(_toast_xml(text, buttons, reminder))
            toast = ToastNotification(doc)
            toast.tag, toast.group = tag[:16], "nc"

            def activated(_sender: Any, args: Any) -> None:
                try:
                    parsed = parse_argument(ToastActivatedEventArgs._from(args).arguments)
                except Exception:  # noqa: BLE001
                    parsed = None
                if parsed:
                    self.on_action(*parsed)

            toast.add_activated(activated)
            self._toasts[tag] = toast
            self._notifier.show(toast)
            return True
        except Exception as exc:  # noqa: BLE001 - fall back to the next channel
            log.info("toast failed (%s)", type(exc).__name__)
            return False

    def info(self, text: str, device_id: str = "") -> str:
        """A message; with a device ID it carries a Stop button."""
        buttons = [("Stop", f"stop:{device_id}")] if device_id else []
        if self._show_toast(f"i{device_id[:15]}", text, buttons, reminder=False):
            return "toast"
        return self._fallback("info", text, device_id)

    def ask(self, request: ConsentRequest) -> str:
        """Accept / Decline for an Ask-me-first request. The window is always told as well."""
        text = ask_text(request.name, request.short_id, request.threads)
        shown = self._show_toast(f"a{request.device_id[:15]}", text,
                                 [("Accept", f"accept:{request.device_id}"), ("Decline", f"decline:{request.device_id}")],
                                 reminder=True)
        if self.window:
            self.window("ask", text, request.device_id)
        return "toast" if shown else "window"

    def withdraw(self, device_id: str) -> None:
        """Remove a question from the Action Center once it is answered or expired."""
        self._toasts.pop(f"a{device_id[:15]}", None)
        if self._notifier is None:
            return
        try:
            from winrt.windows.ui.notifications import ToastNotificationManager

            ToastNotificationManager.history.remove_grouped_tag_with_id(f"a{device_id[:15]}", "nc", AUMID)
        except Exception:  # noqa: BLE001
            pass
