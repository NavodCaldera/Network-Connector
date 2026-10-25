<#
.SYNOPSIS
  Remove the firewall rule created by add-firewall-rule.ps1. Run as administrator on the i7.
#>
#Requires -RunAsAdministrator
$name = "Network Connector Controller (LAN only)"
if (Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue) {
    Remove-NetFirewallRule -DisplayName $name
    Write-Host "Removed '$name'." -ForegroundColor Green
} else {
    Write-Host "Rule '$name' not found; nothing to do."
}
