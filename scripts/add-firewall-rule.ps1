<#
.SYNOPSIS
  Allow workers on your LOCAL network to reach the controller. Run once, as administrator, on the i7.

.DESCRIPTION
  Creates ONE inbound rule:
    - TCP, only the controller port (default 8765)
    - only the Private network profile (not Public, not Domain)
    - only from addresses on your local subnet (RemoteAddress LocalSubnet)
  It does NOT change your network profile, and does not open anything on your router.
  Undo with scripts\remove-firewall-rule.ps1.
#>
#Requires -RunAsAdministrator
param([int]$Port = 8765)
$ErrorActionPreference = "Stop"
$name = "Network Connector Controller (LAN only)"

if (Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue) {
    Write-Host "Rule '$name' already exists; updating its port to $Port."
    Set-NetFirewallRule -DisplayName $name -Profile Private -RemoteAddress LocalSubnet -Action Allow
    Get-NetFirewallRule -DisplayName $name | Get-NetFirewallPortFilter | Set-NetFirewallPortFilter -LocalPort $Port -Protocol TCP
} else {
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Protocol TCP -LocalPort $Port `
        -Profile Private -RemoteAddress LocalSubnet -Action Allow `
        -Description "Lets paired Network Connector workers on the local subnet reach the controller." | Out-Null
    Write-Host "Created rule '$name' (TCP $Port, Private profile, local subnet only)." -ForegroundColor Green
}

$public = Get-NetConnectionProfile | Where-Object NetworkCategory -eq "Public"
if ($public) {
    Write-Host ""
    Write-Host "NOTE: these networks are set to Public, so this rule does NOT apply to them:" -ForegroundColor Yellow
    $public | ForEach-Object { Write-Host "  $($_.Name) ($($_.InterfaceAlias))" -ForegroundColor Yellow }
    Write-Host "If one is your trusted home network, change it to Private in Settings > Network & internet," -ForegroundColor Yellow
    Write-Host "or (as administrator):  Set-NetConnectionProfile -InterfaceAlias '<name>' -NetworkCategory Private" -ForegroundColor Yellow
}

$blocks = Get-NetFirewallRule -Direction Inbound -Action Block -Enabled True -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -like "*python*" }
if ($blocks) {
    Write-Host ""
    Write-Host "WARNING: inbound BLOCK rules for Python exist (often created by clicking Cancel on a Windows" -ForegroundColor Yellow
    Write-Host "firewall pop-up). Block rules win over allow rules. Review them in 'Windows Defender Firewall" -ForegroundColor Yellow
    Write-Host "with Advanced Security' > Inbound Rules:" -ForegroundColor Yellow
    $blocks | ForEach-Object { Write-Host "  $($_.DisplayName)  [$($_.Profile)]" -ForegroundColor Yellow }
}
