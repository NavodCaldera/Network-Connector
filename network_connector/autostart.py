"""Start the app at login for the current user only (HKCU Run key, no admin rights)."""
from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
VALUE = "NetworkConnector"


def _winreg() -> Any:
    import winreg

    return winreg


def command() -> str:
    """The quoted command line Windows runs at login: the windowless Python, starting in the tray."""
    exe = Path(sys.executable)
    windowless = exe.with_name("pythonw.exe")
    if exe.name.lower() == "python.exe" and windowless.is_file():
        exe = windowless
    if '"' in str(exe):
        raise ValueError("unexpected quote in the Python path")
    return f'"{exe}" -m network_connector.app --background'


def supported() -> bool:
    return sys.platform == "win32"


def enabled() -> bool:
    if not supported():
        return False
    reg = _winreg()
    try:
        with reg.OpenKey(reg.HKEY_CURRENT_USER, RUN_KEY) as key:
            value, _kind = reg.QueryValueEx(key, VALUE)
    except OSError:
        return False
    return value == command()


def set_enabled(on: bool) -> bool:
    """Add or remove the login entry. Returns the new state (False where unsupported)."""
    if not supported():
        return False
    reg = _winreg()
    with reg.CreateKey(reg.HKEY_CURRENT_USER, RUN_KEY) as key:
        if on:
            reg.SetValueEx(key, VALUE, 0, reg.REG_SZ, command())
        else:
            try:
                reg.DeleteValue(key, VALUE)
            except OSError:
                pass
    return on
