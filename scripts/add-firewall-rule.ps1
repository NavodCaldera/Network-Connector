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

  -Discovery (helpers, 0.3.0) instead allows the receive-only nearby announcements:
    - UDP only, only the discovery port (default 8766), only for the given python.exe (-Program)
    - the Private profile, from the local subnet
    - with -LocalAddress: also the Public profile, but only to that Wi-Fi Direct group address and only
      from its /24 subnet. Never a blanket "allow python.exe".
#>
#Requires -RunAsAdministrator
param(
    [ValidateRange(1, 65535)][int]$Port = 8765,
    [switch]$WiFiDirect,
    [string]$LocalAddress,
    [string]$RemoteSubnet,
    [switch]$Discovery,
    [ValidateRange(1024, 65535)][int]$DiscoveryPort = 8766,
    [string]$Program
)
$ErrorActionPreference = "Stop"

function Test-DirectAddress([string]$Address) {
    $parsed = $null
    if ($Address -notmatch '^\d{1,3}(\.\d{1,3}){3}$' -or
        -not [System.Net.IPAddress]::TryParse($Address, [ref]$parsed) -or
        $parsed.ToString() -ne $Address) { throw "LocalAddress must be a canonical IPv4 address." }
    $octets = @($Address.Split('.') | ForEach-Object { [int]$_ })
    $private = $octets[0] -eq 10 -or ($octets[0] -eq 172 -and $octets[1] -ge 16 -and $octets[1] -le 31) -or
        ($octets[0] -eq 192 -and $octets[1] -eq 168) -or ($octets[0] -eq 169 -and $octets[1] -eq 254)
    if (-not $private -or $octets[3] -in @(0, 255)) { throw "Use a private or link-local address on the direct interface." }
    if (-not (Get-NetIPAddress -AddressFamily IPv4 -IPAddress $Address -ErrorAction SilentlyContinue)) {
        throw "LocalAddress is not assigned to this computer. Start the direct link first."
    }
    return "{0}.{1}.{2}.0/24" -f $octets[0], $octets[1], $octets[2]
}

if ($Discovery) {
    if ($WiFiDirect -or $RemoteSubnet) { throw "-Discovery can't be combined with -WiFiDirect or -RemoteSubnet." }
    if (-not $Program -or $Program -match '[*?"]' -or -not [System.IO.Path]::IsPathRooted($Program) -or
        (Split-Path -Leaf $Program) -ne "python.exe" -or -not (Test-Path -LiteralPath $Program -PathType Leaf)) {
        throw "Program must be the full path of the base python.exe (sys._base_executable), not a launcher or wildcard."
    }
    $Program = (Resolve-Path -LiteralPath $Program).Path
    $lanName = "Network Connector Discovery (LAN)"
    if (Get-NetFirewallRule -DisplayName $lanName -ErrorAction SilentlyContinue) {
        Set-NetFirewallRule -DisplayName $lanName -Direction Inbound -Profile Private -Enabled True -Action Allow -RemoteAddress LocalSubnet -Program $Program
        Get-NetFirewallRule -DisplayName $lanName | Get-NetFirewallPortFilter | Set-NetFirewallPortFilter -LocalPort $DiscoveryPort -Protocol UDP
    } else {
        New-NetFirewallRule -DisplayName $lanName -Direction Inbound -Protocol UDP -LocalPort $DiscoveryPort `
            -Profile Private -RemoteAddress LocalSubnet -Program $Program -Action Allow `
            -Description "Receive-only, signed nearby announcements for Network Connector helpers." | Out-Null
    }
    Write-Host "Allowed UDP $DiscoveryPort for $Program on Private networks, from the local subnet." -ForegroundColor Green
    if ($LocalAddress) {
        $subnet = Test-DirectAddress $LocalAddress
        $directName = "Network Connector Discovery (Wi-Fi Direct)"
        if (Get-NetFirewallRule -DisplayName $directName -ErrorAction SilentlyContinue) {
            Set-NetFirewallRule -DisplayName $directName -Direction Inbound -Profile Public -Enabled True -Action Allow -LocalAddress $LocalAddress -RemoteAddress $subnet -Program $Program
            Get-NetFirewallRule -DisplayName $directName | Get-NetFirewallPortFilter | Set-NetFirewallPortFilter -LocalPort $DiscoveryPort -Protocol UDP
        } else {
            New-NetFirewallRule -DisplayName $directName -Direction Inbound -Protocol UDP -LocalPort $DiscoveryPort `
                -Profile Public -LocalAddress $LocalAddress -RemoteAddress $subnet -Program $Program -Action Allow `
                -Description "Nearby announcements on the Wi-Fi Direct group subnet only." | Out-Null
        }
        Write-Host "Also allowed UDP $DiscoveryPort to $LocalAddress from $subnet on the Public direct link." -ForegroundColor Green
    }
    Write-Host "An existing inbound BLOCK rule for python.exe still wins over these. See docs\JOINING.md." -ForegroundColor Yellow
    return
}
if ($Program) { throw "Program requires -Discovery." }
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
