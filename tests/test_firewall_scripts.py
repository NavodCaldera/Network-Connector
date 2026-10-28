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
