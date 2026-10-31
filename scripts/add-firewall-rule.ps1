<#
.SYNOPSIS
  Let the other PC reach "nc host" / "nc listen". Run once, as administrator, on the PC that hosts.

.DESCRIPTION
  One inbound rule: TCP, only the Hello port (default 8765), only from the local subnet.
  It covers the Public profile too, because Windows usually puts a Wi-Fi Direct group there.
  Undo with scripts\remove-firewall-rule.ps1.
#>
#Requires -RunAsAdministrator
param([ValidateRange(1, 65535)][int]$Port = 8765)
$ErrorActionPreference = "Stop"
$name = "Network Connector Hello (TCP $Port)"
Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow -Protocol TCP -LocalPort $Port `
    -Profile Private, Public -RemoteAddress LocalSubnet | Out-Null
Write-Host "Added: $name (local subnet only)."
