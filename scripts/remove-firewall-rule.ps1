#Requires -RunAsAdministrator
param([ValidateRange(1, 65535)][int]$Port = 8765)
$name = "Network Connector Hello (TCP $Port)"
Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
Write-Host "Removed: $name"
