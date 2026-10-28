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
param(
    [ValidateRange(1, 65535)][int]$Port = 8765,
    [switch]$WiFiDirect,
    [string]$LocalAddress,
    [string]$RemoteSubnet
)
$ErrorActionPreference = "Stop"
if ($WiFiDirect) {
    $parsed = $null
    if ($LocalAddress -notmatch '^\d{1,3}(\.\d{1,3}){3}$' -or
        -not [System.Net.IPAddress]::TryParse($LocalAddress, [ref]$parsed) -or
        $parsed.ToString() -ne $LocalAddress) { throw "LocalAddress must be a canonical IPv4 address." }
    $octets = @($LocalAddress.Split('.') | ForEach-Object { [int]$_ })
    $private = $octets[0] -eq 10 -or ($octets[0] -eq 172 -and $octets[1] -ge 16 -and $octets[1] -le 31) -or
        ($octets[0] -eq 192 -and $octets[1] -eq 168) -or ($octets[0] -eq 169 -and $octets[1] -eq 254)
    if (-not $private -or $octets[3] -in @(0, 255)) { throw "Use a private or link-local controller address on the direct interface." }
    $subnet = "{0}.{1}.{2}.0/24" -f $octets[0], $octets[1], $octets[2]
    if ($RemoteSubnet -and $RemoteSubnet -cne $subnet) { throw "RemoteSubnet must match the LocalAddress /24: $subnet" }
    if (-not (Get-NetIPAddress -AddressFamily IPv4 -IPAddress $LocalAddress -ErrorAction SilentlyContinue)) {
        throw "LocalAddress is not assigned to this computer. Start the direct link first."
    }
    $directName = "Network Connector Controller (Wi-Fi Direct)"
    if (Get-NetFirewallRule -DisplayName $directName -ErrorAction SilentlyContinue) {
        Set-NetFirewallRule -DisplayName $directName -Direction Inbound -Profile Any -Enabled True -Action Allow -LocalAddress $LocalAddress -RemoteAddress $subnet
        Get-NetFirewallRule -DisplayName $directName | Get-NetFirewallPortFilter | Set-NetFirewallPortFilter -LocalPort $Port -Protocol TCP
    } else {
        New-NetFirewallRule -DisplayName $directName -Direction Inbound -Protocol TCP -LocalPort $Port `
            -Profile Any -LocalAddress $LocalAddress -RemoteAddress $subnet -Action Allow `
            -Description "Paired workers on the direct Wi-Fi subnet only." | Out-Null
    }
    Write-Host "Allowed TCP $Port to $LocalAddress from $subnet on the direct link. The LAN rule is unchanged." -ForegroundColor Green
    return
}
if ($LocalAddress -or $RemoteSubnet) { throw "LocalAddress and RemoteSubnet require -WiFiDirect." }
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
