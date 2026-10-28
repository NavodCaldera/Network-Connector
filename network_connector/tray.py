"""Tray icon (pystray). Menu clicks never touch tk: they only put an action on a queue that tk polls.

Phase 6 spike: `Icon.run_detached()` keeps pystray's message loop on its own thread, and `after(100)`
polling on the tk side hands every click to the main thread.
"""
from __future__ import annotations

import logging
import queue
from typing import Any, Callable

log = logging.getLogger("network_connector.tray")

MENU_ACTIONS = ("pause", "stop", "open", "quit")
COLORS = {"ready": "#2a9d5b", "busy": "#2f6fdf", "paused": "#a15c00", "stopped": "#888888"}


def make_image(state: str = "ready") -> Any:
    from PIL import Image, ImageDraw

    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw.ellipse((4, 4, 60, 60), fill=COLORS.get(state, COLORS["stopped"]))
    draw.text((21, 24), "NC", fill="white")
    return img


class Tray:
    """Status, Pause, Stop helping, Open and Quit. `status()` and `paused()` are read from pystray's thread,
    so they must only read plain attributes."""

    def __init__(self, status: Callable[[], str], paused: Callable[[], bool]) -> None:
        self.actions: queue.Queue[str] = queue.Queue()
        self._status, self._paused = status, paused
        self._icon: Any = None
        self._state = ""

    @property
    def running(self) -> bool:
        return self._icon is not None

    @property
    def can_notify(self) -> bool:
        return bool(self._icon is not None and getattr(self._icon, "HAS_NOTIFICATION", False))

    def _put(self, action: str) -> Callable[..., None]:
        def handler(_icon: Any = None, _item: Any = None) -> None:
            self.actions.put(action)
        return handler

    def start(self) -> bool:
        """False when pystray is not installed (the app then simply has no tray icon)."""
        try:
            import pystray
        except ImportError:
            log.info("pystray is not installed; running without a tray icon (pip install network-connector[app])")
            return False
        menu = pystray.Menu(
            pystray.MenuItem(lambda _item: self._status(), None, enabled=False),
            pystray.MenuItem("Pause", self._put("pause"), checked=lambda _item: self._paused()),
            pystray.MenuItem("Stop helping", self._put("stop")),
            pystray.MenuItem("Open", self._put("open"), default=True),
            pystray.MenuItem("Quit", self._put("quit")),
        )
        try:
            self._icon = pystray.Icon("network-connector", make_image(), "Network Connector", menu)
            self._icon.run_detached()
        except Exception as exc:  # noqa: BLE001 - no tray on this desktop
            log.info("tray icon unavailable: %s", exc)
            self._icon = None
            return False
        return True

    def set_state(self, state: str) -> None:
        if self._icon is None or state == self._state:
            return
        self._state = state
        try:
            self._icon.icon = make_image(state)
            self._icon.title = f"Network Connector — {self._status()}"[:120]
            self._icon.update_menu()
        except Exception:  # noqa: BLE001
            pass

    def notify(self, text: str) -> None:
        if self._icon is not None:
            self._icon.notify(text[:250], "Network Connector")

    def poll(self, handle: Callable[[str], None]) -> None:
        """Run queued menu actions on the caller's (tk) thread."""
        while True:
            try:
                action = self.actions.get_nowait()
            except queue.Empty:
                return
            if action in MENU_ACTIONS:
                handle(action)

    def stop(self) -> None:
        icon, self._icon = self._icon, None
        if icon is not None:
            try:
                icon.stop()
            except Exception:  # noqa: BLE001
                pass
