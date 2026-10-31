"""Make sure Windows Firewall lets the other PC's Hello in, with one admin prompt the user approves.

A Block rule for python.exe (Windows creates one when its "allow this app?" pop-up is cancelled) beats any Allow
rule, so the fix disables those Block rules for this Python only and adds the Hello port rule.
"""
from __future__ import annotations

import subprocess
import sys

from .hello import PORT

RULE_NAME = f"Network Connector Hello (TCP {PORT})"
_NO_WINDOW = 0x08000000


def _pythons() -> list[str]:
    """python.exe and pythonw.exe of the real interpreter: the CLI listens with one, the window with the other."""
    import os

    folder = os.path.dirname(getattr(sys, "_base_executable", "") or sys.executable)
    return [os.path.join(folder, name) for name in ("python.exe", "pythonw.exe")]


def _ps(script: str, timeout: float = 30) -> subprocess.CompletedProcess:
    return subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script], capture_output=True,
                          text=True, errors="replace", timeout=timeout, creationflags=_NO_WINDOW)


def _quote(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def _find_blocks() -> str:
    return (f"$py = @({', '.join(_quote(p) for p in _pythons())}); "
            "Get-NetFirewallRule -Direction Inbound -Action Block -Enabled True | "
            "Where-Object { $py -contains ($_ | Get-NetFirewallApplicationFilter).Program }")


def problems() -> list[str]:
    """What stops the Hello getting in. Empty when the firewall is fine (or this is not Windows)."""
    if sys.platform != "win32":
        return []
    script = (f"$b = @({_find_blocks()}).Count; "
              f"$a = @(Get-NetFirewallRule -DisplayName {_quote(RULE_NAME)} -ErrorAction SilentlyContinue | "
              "Where-Object { $_.Enabled -eq 'True' }).Count; "
              "\"$b $a\"")
    try:
        blocks, allows = (int(x) for x in _ps(script).stdout.split())
    except (ValueError, OSError, subprocess.TimeoutExpired):
        return []  # can't tell; don't nag
    out = []
    if blocks:
        out.append(f"{blocks} Windows Firewall rule(s) block Python on this PC")
    if not allows:
        out.append(f"no firewall rule allows TCP {PORT}")
    return out


def fix() -> bool:
    """Show the Windows admin prompt once; returns True when the firewall is fine afterwards."""
    inner = (f"{_find_blocks()} | Disable-NetFirewallRule; "
             f"Get-NetFirewallRule -DisplayName {_quote(RULE_NAME)} -ErrorAction SilentlyContinue | Remove-NetFirewallRule; "
             f"New-NetFirewallRule -DisplayName {_quote(RULE_NAME)} -Direction Inbound -Action Allow -Protocol TCP "
             f"-LocalPort {PORT} -Profile Private,Public -RemoteAddress LocalSubnet | Out-Null")
    encoded = __import__("base64").b64encode(inner.encode("utf-16-le")).decode()
    try:
        _ps("Start-Process powershell -Verb RunAs -Wait -WindowStyle Hidden "
            f"-ArgumentList '-NoProfile -EncodedCommand {encoded}'", timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return not problems()
