"""Interactive hardware spike only; not imported by the application."""
from __future__ import annotations

import argparse
import getpass
import json
import re
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from xml.sax.saxutils import escape

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from network_connector.security import protect_path


def capture(role: str, stage: str) -> None:
    commands = [
        ["netsh", "wlan", "show", "drivers"],
        ["netsh", "wlan", "show", "wirelesscapabilities"],
        ["netsh", "wlan", "show", "interfaces"],
        ["powershell", "-NoProfile", "-Command", "Get-NetAdapter -IncludeHidden | Format-List Name,InterfaceDescription,Status"],
        ["powershell", "-NoProfile", "-Command", "Get-NetIPConfiguration -All | Format-List *"],
        ["powershell", "-NoProfile", "-Command", "Get-NetConnectionProfile | Format-List *"],
    ]
    rows = []
    for command in commands:
        try:
            proc = subprocess.run(command, capture_output=True, text=True, errors="replace", timeout=30)
            rows.append({"command": command, "exit": proc.returncode, "stdout": proc.stdout, "stderr": proc.stderr})
        except (OSError, subprocess.TimeoutExpired) as exc:
            rows.append({"command": command, "error": str(exc)})
    out = Path(__file__).parent / f"{role}-{stage}-{time.time_ns()}.json"
    out.write_text(json.dumps(rows, indent=2), encoding="utf-8")
    print(f"Diagnostic output: {out}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("role", choices=["controller", "worker"])
    parser.add_argument("--mode", choices=["inspect", "direct", "hotspot"], default="inspect")
    args = parser.parse_args()
    capture(args.role, "before")
    if args.mode == "inspect":
        return
    print("This interactive test changes the Wi-Fi connection. Keep both laptops available.")
    if input("Type START to continue: ") != "START":
        return
    if args.mode == "hotspot" and args.role == "controller":
        input("Start Mobile Hotspot in Windows Settings with no internet to share; press Enter to capture.")
        capture(args.role, "hotspot")
        input("Record whether it worked, and stop the hotspot manually when finished. Enter to exit.")
        return
    ssid = input("SSID (DIRECT-NC- followed by four letters/digits for direct mode): ").strip()
    password = getpass.getpass("Passphrase (enter the same on both laptops; never written to the report): ")
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,32}", ssid) or not re.fullmatch(r"[A-Za-z0-9]{8,63}", password):
        raise SystemExit("Use an SSID of letters/digits/_/- and an 8–63 character alphanumeric passphrase.")
    if args.mode == "direct" and not re.fullmatch(r"DIRECT-NC-[A-Za-z0-9]{4}", ssid):
        raise SystemExit("Direct mode requires DIRECT-NC- plus four letters/digits.")
    if args.role == "controller":
        from winrt.windows.devices.wifidirect import WiFiDirectAdvertisementPublisher
        from winrt.windows.security.credentials import PasswordCredential

        publisher = WiFiDirectAdvertisementPublisher()
        publisher.advertisement.is_autonomous_group_owner_enabled = True
        legacy = publisher.advertisement.legacy_settings
        legacy.is_enabled = True
        legacy.ssid = ssid
        credential = PasswordCredential()
        credential.password = password
        legacy.passphrase = credential
        try:
            publisher.start()
            time.sleep(5)
            print(f"Publisher status: {publisher.status}")
            capture(args.role, "direct")
            input("Join from the worker; check home Wi-Fi still works. Enter to stop publisher.")
        finally:
            publisher.stop()
        capture(args.role, "stopped")
    else:
        profile_name = "NC-spike-" + uuid.uuid4().hex[:12]
        xml = f'''<?xml version="1.0"?>
<WLANProfile xmlns="http://www.microsoft.com/networking/WLAN/profile/v1"><name>{profile_name}</name>
<SSIDConfig><SSID><name>{escape(ssid)}</name></SSID></SSIDConfig><connectionType>ESS</connectionType>
<connectionMode>manual</connectionMode><MSM><security><authEncryption><authentication>WPA2PSK</authentication>
<encryption>AES</encryption><useOneX>false</useOneX></authEncryption><sharedKey><keyType>passPhrase</keyType>
<protected>false</protected><keyMaterial>{password}</keyMaterial></sharedKey></security></MSM></WLANProfile>'''
        with tempfile.TemporaryDirectory(prefix="nc-link-spike-") as directory:
            profile = Path(directory) / "profile.xml"
            profile.touch()
            protect_path(profile)
            profile.write_text(xml, encoding="utf-8")
            subprocess.run(["netsh", "wlan", "add", "profile", f"filename={profile}", "user=current"], check=True, timeout=30)
        try:
            subprocess.run(["netsh", "wlan", "connect", f"name={profile_name}", f"ssid={ssid}"], check=True, timeout=30)
            input("Wait until connected, then Enter to record IP, gateway and network category.")
            capture(args.role, args.mode)
            host = input("Controller's direct-link IPv4 address for ping (blank to skip): ").strip()
            if host:
                import ipaddress
                host = str(ipaddress.IPv4Address(host))
                subprocess.run(["ping", "-n", "10", host], timeout=30)
            input("Complete manual checks in REPORT.md; Enter to remove this test profile.")
        finally:
            subprocess.run(["netsh", "wlan", "delete", "profile", f"name={profile_name}"], timeout=30)


if __name__ == "__main__":
    main()
