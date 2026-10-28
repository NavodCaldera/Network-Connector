"""Shared helpers for the Phase 6 network spikes. Throwaway code, not imported by the app."""
from __future__ import annotations

import base64
import hashlib
import re
import socket
import subprocess
import time
from pathlib import Path

OUT = Path(__file__).resolve().parent / "out"
SSID_RE = re.compile(r"DIRECT-NC-[A-Z0-9]{4}")
# PLAN §15.3: the Wi-Fi password is public by design (derivable from the SSID). Spike salt only.
SPIKE_SALT = b"network-connector/0.3/spike/wifi-direct"

# Constant command lines only. No SSID, address or password is ever interpolated into PowerShell.
STATE_COMMANDS = [
    ["netsh", "wlan", "show", "interfaces"],
    ["powershell", "-NoProfile", "-NonInteractive", "-Command",
     "Get-NetAdapter -IncludeHidden | Where-Object Status -eq 'Up' | Format-Table -AutoSize Name,InterfaceDescription,ifIndex"],
    ["powershell", "-NoProfile", "-NonInteractive", "-Command",
     "Get-NetIPAddress -AddressFamily IPv4 | Format-Table -AutoSize InterfaceAlias,IPAddress,PrefixLength"],
    ["powershell", "-NoProfile", "-NonInteractive", "-Command",
     "Get-NetIPConfiguration | Format-List InterfaceAlias,IPv4Address,@{n='Gateway';e={$_.IPv4DefaultGateway.NextHop}}"],
    ["powershell", "-NoProfile", "-NonInteractive", "-Command",
     "Get-NetConnectionProfile | Format-Table -AutoSize Name,InterfaceAlias,NetworkCategory,IPv4Connectivity"],
]

FIREWALL_COMMAND = ["powershell", "-NoProfile", "-NonInteractive", "-Command",
                    "Get-NetFirewallProfile | Format-Table Name,Enabled,DefaultInboundAction,NotifyOnListen; "
                    "Get-NetFirewallApplicationFilter | Where-Object { $_.Program -like '*python*' } | ForEach-Object { "
                    "$r = $_ | Get-NetFirewallRule; $p = $r | Get-NetFirewallPortFilter; [pscustomobject]@{Name=$r.DisplayName;"
                    "Dir=$r.Direction;Action=$r.Action;Enabled=$r.Enabled;Profile=$r.Profile;Proto=$p.Protocol;Port=$p.LocalPort;"
                    "Program=$_.Program} } | Format-Table -AutoSize -Wrap"]


def derive_passphrase(ssid: str) -> str:
    if not SSID_RE.fullmatch(ssid):
        raise ValueError("SSID must be DIRECT-NC- plus 4 capital letters/digits")
    raw = hashlib.pbkdf2_hmac("sha256", ssid.encode(), SPIKE_SALT, 200_000, 12)
    return base64.b32encode(raw).decode().rstrip("=")  # 20 chars A-Z2-7


def run(cmd: list[str], timeout: float = 30) -> str:
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, errors="replace", timeout=timeout)
        return f"$ {' '.join(cmd[:4])}{' ...' if len(cmd) > 4 else ''}  (exit {p.returncode})\n{p.stdout}{p.stderr}"
    except (OSError, subprocess.TimeoutExpired) as exc:
        return f"$ {cmd[:3]} failed: {exc}\n"


def capture(stage: str, firewall: bool = False) -> str:
    text = f"===== {stage} {time.strftime('%Y-%m-%d %H:%M:%S')} host={socket.gethostname()} =====\n"
    for cmd in STATE_COMMANDS + ([FIREWALL_COMMAND] if firewall else []):
        text += run(cmd) + "\n"
    print(text, flush=True)
    return text


def connected_ssid() -> str:
    out = subprocess.run(["netsh", "wlan", "show", "interfaces"], capture_output=True, text=True, errors="replace").stdout
    m = re.search(r"^\s+SSID\s+:\s(.+)$", out, re.M)
    return m.group(1).strip() if m else ""


def connected_profile() -> str:
    out = subprocess.run(["netsh", "wlan", "show", "interfaces"], capture_output=True, text=True, errors="replace").stdout
    m = re.search(r"^\s+Profile\s+:\s(.+)$", out, re.M)
    return m.group(1).strip() if m else ""
