<#
.SYNOPSIS
  Remove the firewall rule created by add-firewall-rule.ps1. Run as administrator on the i7.
#>
#Requires -RunAsAdministrator
param([switch]$WiFiDirect)
$names = if ($WiFiDirect) { @("Network Connector Controller (Wi-Fi Direct)") } else {
    @("Network Connector Controller (LAN only)", "Network Connector Controller (Wi-Fi Direct)")
}
foreach ($name in $names) {
    if (Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue) {
        Remove-NetFirewallRule -DisplayName $name
        Write-Host "Removed '$name'." -ForegroundColor Green
    } else {
        Write-Host "Rule '$name' not found; nothing to do."
    }
}
