#!/usr/bin/env bash
# Run under tools/g1-locked.sh from the checkout. Use -Observe to sample Explorer.
set -euo pipefail

if [[ $# -gt 1 || ( $# -eq 1 && $1 != -Observe ) ]]; then
    printf '%s\n' 'Usage: tools/g1-notification-state.sh [-Observe]' >&2
    exit 2
fi

dir='C:\explorust\notification-state-probe'
task='explorust-notification-state-probe'
guest() { timeout 90 os win run g1 --ps -- "$1"; }
cleanup() {
    guest "
        Stop-ScheduledTask -TaskName '$task' -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName '$task' -Confirm:\$false -ErrorAction SilentlyContinue
        Remove-Item '$dir' -Recurse -Force -ErrorAction SilentlyContinue
    " >/dev/null || true
}
trap cleanup EXIT
guest "New-Item -ItemType Directory -Force '$dir' | Out-Null" >/dev/null
timeout 90 os win push g1 tools/g1-notification-state.ps1 "$dir" >/dev/null
status=0
out=$(guest "
    \$ErrorActionPreference = 'Stop'
    Remove-Item '$dir\notification-state.log' -ErrorAction SilentlyContinue
    \$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File $dir\g1-notification-state.ps1 ${1:-}'
    \$principal = New-ScheduledTaskPrincipal -UserId \"\$env:COMPUTERNAME\agent\" -LogonType Interactive
    Register-ScheduledTask -TaskName '$task' -Action \$action -Principal \$principal -Force | Out-Null
    Start-ScheduledTask -TaskName '$task'
    \$path = '$dir\notification-state.log'
    for (\$i = 0; \$i -lt 100; \$i++) {
        if ((Test-Path \$path) -and (Select-String -Quiet -Pattern '^DONE$' -Path \$path)) {
            Get-Content \$path
            exit 0
        }
        Start-Sleep -Milliseconds 200
    }
    if (Test-Path \$path) { Get-Content \$path }
    Get-ScheduledTaskInfo '$task' | Format-List LastRunTime,LastTaskResult
    exit 2
") || status=$?
log=$(awk '/^log: / {print $2}' <<< "$out")
if [[ ! -f "$log" ]]; then
    printf '%s\n' "$out" >&2
    exit 2
fi
cat "$log"
if [[ $status != 0 ]] || ! rg -q '^DONE\r?$' "$log" || rg -q '^ERROR ' "$log"; then
    exit 2
fi
if rg -q '^FAIL ' "$log"; then
    exit 1
fi
