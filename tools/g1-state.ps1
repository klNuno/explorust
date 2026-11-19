# Prints the explorust state of the guest's logged-on user: shell processes, the per-user
# Winlogon Shell value, the shell window owner and the tail of the guard and shell logs.
# Run as SYSTEM: os win run g1 --ps -- "& C:\explorust\g1-state.ps1"
param([string]$User = 'agent', [int]$Tail = 12)
$sid = (New-Object Security.Principal.NTAccount($User)).Translate([Security.Principal.SecurityIdentifier]).Value
$wl = Get-ItemProperty "Registry::HKEY_USERS\$sid\Software\Microsoft\Windows NT\CurrentVersion\Winlogon" -EA SilentlyContinue
$st = Get-ItemProperty "Registry::HKEY_USERS\$sid\Software\explorust" -EA SilentlyContinue
"user Shell = $($wl.Shell)"
"LastGoodBuild = $($st.LastGoodBuild)"
$v = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
"windows = $($v.CurrentBuildNumber).$($v.UBR)"
Get-Process explorust-guard, explorust, explorust-keys, explorer, ApplicationFrameHost, CalculatorApp, SystemSettings -EA SilentlyContinue |
    Select-Object Name, Id, SessionId, @{n = 'MB'; e = { [math]::Round($_.PrivateMemorySize64 / 1MB, 1) } }, StartTime |
    Format-Table -AutoSize | Out-String -Width 200
$dir = "C:\Users\$User\AppData\Local\explorust"
foreach ($f in 'guard.log', 'shell.log') {
    "--- $f"
    if (Test-Path "$dir\$f") { Get-Content "$dir\$f" -Tail $Tail } else { '(none)' }
}
