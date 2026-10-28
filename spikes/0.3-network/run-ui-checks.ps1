# Desktop checks that need a person looking at the screen. Run on each PC, normal (not elevated) PowerShell.
#   -Check tray   : tray icon for 60 s; right-click it, try Pause / Stop helping / Open, then Quit.
#   -Check toast  : two toasts; click "Accept" on the second within 60 s.
#   -Check radio  : first turn Wi-Fi OFF yourself (Quick Settings Wi-Fi button, NOT airplane mode), then run.
#                   Repeat once with airplane mode ON.
param([Parameter(Mandatory)][ValidateSet('tray', 'toast', 'radio')][string]$Check)
$ErrorActionPreference = 'Stop'
$py = "$PSScriptRoot\.venv\Scripts\python.exe"
$out = Join-Path $PSScriptRoot 'out'
New-Item -ItemType Directory -Force $out | Out-Null
$log = Join-Path $out ("ui-{0}-{1}-{2}.txt" -f $Check, $env:COMPUTERNAME, (Get-Date -Format 'yyyyMMdd-HHmmss'))
switch ($Check) {
    'tray'  { & $py -u "$PSScriptRoot\tray_spike.py" --interactive 2>&1 | Tee-Object -FilePath $log }
    'toast' { & $py -u "$PSScriptRoot\toast_spike.py" --wait 60 2>&1 | Tee-Object -FilePath $log }
    'radio' { & $py -u "$PSScriptRoot\radio_spike.py" 2>&1 | Tee-Object -FilePath $log
              & $py -u "$PSScriptRoot\radio_spike.py" --turn-on 2>&1 | Tee-Object -FilePath $log -Append }
}
Write-Host "Saved $log. Also write down what you SAW (icon, balloon text, toast title, button result)."
