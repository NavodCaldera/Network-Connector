"""Run firewall validation against stubbed PowerShell cmdlets; never alter the firewall."""
import json
from pathlib import Path
import subprocess
import sys

import pytest


@pytest.mark.skipif(sys.platform != "win32", reason="Windows PowerShell script")
@pytest.mark.parametrize("address,subnet,valid", [
    ("192.168.137.1", "192.168.137.0/24", True),
    ("169.254.10.1", "169.254.10.0/24", True),
    ("0.0.0.0", "0.0.0.0/24", False),
    ("8.8.8.8", "8.8.8.0/24", False),
    ("192.168.137.1", "0.0.0.0/0", False),
    ("192.168.137.1;Write-Output bad", "192.168.137.0/24", False),
])
def test_direct_rule_is_narrow_and_rejects_bad_addresses(address, subnet, valid):
    path = str(Path(__file__).resolve().parents[1] / "scripts" / "add-firewall-rule.ps1").replace("'", "''")
    def literal(text):
        return "'" + text.replace("'", "''") + "'"
    harness = r"""
$script:created=$null
function Get-NetIPAddress { param($AddressFamily,$IPAddress,$ErrorAction) [pscustomobject]@{IPAddress=$IPAddress} }
function Get-NetFirewallRule { param($DisplayName,$ErrorAction) $null }
function New-NetFirewallRule {
 param($DisplayName,$Direction,$Protocol,$LocalPort,$Profile,$LocalAddress,$RemoteAddress,$Action,$Description)
 $script:created=[pscustomobject]@{name=$DisplayName;profile=$Profile;local=$LocalAddress;remote=$RemoteAddress;protocol=$Protocol;port=$LocalPort;action=$Action}
}
$ok=$true
try {
 $source=(Get-Content -LiteralPath SCRIPT_PATH -Raw) -replace '(?m)^#Requires.*$', ''
 & ([scriptblock]::Create($source)) -WiFiDirect -Port 9876 -LocalAddress ADDRESS -RemoteSubnet SUBNET | Out-Null
} catch { $ok=$false }
@{ok=$ok;rule=$script:created}|ConvertTo-Json -Compress
""".replace("SCRIPT_PATH", literal(path)).replace("ADDRESS", literal(address)).replace("SUBNET", literal(subnet))
    proc = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", harness],
                          capture_output=True, text=True, timeout=20)
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout.strip().splitlines()[-1])
    assert result["ok"] is valid
    if valid:
        assert result["rule"] == {"name": "Network Connector Controller (Wi-Fi Direct)", "profile": "Any",
                                  "local": address, "remote": subnet, "protocol": "TCP", "port": 9876, "action": "Allow"}
    else:
        assert result["rule"] is None


def test_discovery_rule_is_udp_only_for_one_program_and_scoped():
    """Static check, on every platform: the -Discovery branch never opens TCP, any program, or a blanket Public rule."""
    import re

    text = (Path(__file__).resolve().parents[1] / "scripts" / "add-firewall-rule.ps1").read_text(encoding="utf-8")
    branch = text[text.index("if ($Discovery) {"):text.index('if ($Program) { throw')]
    joined = re.sub(r"`\s*\n\s*", " ", branch)  # PowerShell line continuations
    rules = re.findall(r"New-NetFirewallRule[^\n]*", joined)
    assert len(rules) == 2
    for rule in rules:
        assert "-Protocol UDP" in rule and "-LocalPort $DiscoveryPort" in rule and "-Program $Program" in rule
        assert "Any" not in rule and "TCP" not in rule
    assert "-Profile Private -RemoteAddress LocalSubnet" in rules[0]
    assert "-Profile Public -LocalAddress $LocalAddress -RemoteAddress $subnet" in rules[1]
    remove = (Path(__file__).resolve().parents[1] / "scripts" / "remove-firewall-rule.ps1").read_text(encoding="utf-8")
    assert "Network Connector Discovery (LAN)" in remove and "Network Connector Discovery (Wi-Fi Direct)" in remove


def test_gui_discovery_command_passes_only_validated_values(monkeypatch, tmp_path):
    from network_connector import link_gui

    exe = tmp_path / "python.exe"
    exe.write_text("")
    monkeypatch.setattr(link_gui.sys, "_base_executable", str(exe), raising=False)
    command = link_gui.discovery_firewall_command(8766, "192.168.137.1")
    assert "'-Discovery'" in command and "'-DiscoveryPort','8766'" in command and "'-LocalAddress','192.168.137.1'" in command
    assert str(exe) in command
    with pytest.raises(ValueError):
        link_gui.discovery_firewall_command(8766, "192.168.137.1;calc")
    with pytest.raises(ValueError):
        link_gui.discovery_firewall_command(80, None)
    bad = tmp_path / "py$thon" / "python.exe"
    bad.parent.mkdir()
    bad.write_text("")
    monkeypatch.setattr(link_gui.sys, "_base_executable", str(bad), raising=False)
    with pytest.raises(RuntimeError):
        link_gui.discovery_firewall_command(8766, None)


@pytest.mark.skipif(sys.platform != "win32", reason="Windows PowerShell script")
@pytest.mark.parametrize("program,address,valid", [
    ("REAL", "", True),
    ("REAL", "192.168.137.1", True),
    ("C:\\Windows\\System32\\cmd.exe", "", False),
    ("python.exe", "", False),
    ("REAL", "8.8.8.8", False),
    ("REAL", "192.168.137.1;Write-Output bad", False),
])
def test_discovery_rule_harness(program, address, valid):
    path = str(Path(__file__).resolve().parents[1] / "scripts" / "add-firewall-rule.ps1").replace("'", "''")
    program = sys._base_executable if program == "REAL" else program
    def literal(text):
        return "'" + text.replace("'", "''") + "'"
    harness = r"""
$script:created=@()
function Get-NetIPAddress { param($AddressFamily,$IPAddress,$ErrorAction) [pscustomobject]@{IPAddress=$IPAddress} }
function Get-NetFirewallRule { param($DisplayName,$ErrorAction) $null }
function New-NetFirewallRule {
 param($DisplayName,$Direction,$Protocol,$LocalPort,$Profile,$LocalAddress,$RemoteAddress,$Program,$Action,$Description)
 $script:created+=[pscustomobject]@{name=$DisplayName;profile=$Profile;local=$LocalAddress;remote=$RemoteAddress;protocol=$Protocol;port=$LocalPort;program=$Program}
}
$ok=$true
try {
 $source=(Get-Content -LiteralPath SCRIPT_PATH -Raw) -replace '(?m)^#Requires.*$', ''
 $extra=@{}; if (ADDRESS) { $extra.LocalAddress=ADDRESS }
 & ([scriptblock]::Create($source)) -Discovery -Program PROGRAM @extra | Out-Null
} catch { $ok=$false }
@{ok=$ok;rules=@($script:created)}|ConvertTo-Json -Compress -Depth 4
""".replace("SCRIPT_PATH", literal(path)).replace("ADDRESS", literal(address)).replace("PROGRAM", literal(program))
    proc = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", harness],
                          capture_output=True, text=True, timeout=20)
    assert proc.returncode == 0, proc.stderr
    result = json.loads(proc.stdout.strip().splitlines()[-1])
    assert result["ok"] is valid
    if valid:
        assert {r["protocol"] for r in result["rules"]} == {"UDP"}
        assert {r["port"] for r in result["rules"]} == {8766}
        assert len(result["rules"]) == (2 if address else 1)
        assert result["rules"][0]["profile"] == "Private"
        if address:
            assert result["rules"][1] == {"name": "Network Connector Discovery (Wi-Fi Direct)", "profile": "Public",
                                          "local": address, "remote": "192.168.137.0/24", "protocol": "UDP",
                                          "port": 8766, "program": result["rules"][0]["program"]}
