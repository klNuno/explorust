#!/usr/bin/env bash
# Builds and deploys the shell and guard. The running shell's guard watcher adopts the
# on-disk guard before the shell reloads. Run under tools/g1-locked.sh.
# The notify helper (explorust-notify.exe, run with the identity of its sparse package) and that
# package (explorust-notify.msix, registered by --install) go along: the running helper locks its
# exe, so it is renamed aside (explorust-notify.old<n>.exe, deleted by a later deploy) before the copy.
#
# Installed mode (`explorust-guard --install` run elevated, the shell in C:\Program Files\explorust):
# explorust-keys.exe (the keyboard hook) is built with uiAccess (EXPLORUST_UIACCESS=1) and signed with
# tools/authenticode-sign.py, explorust.exe stays plain; the files go to C:\explorust as before, then a SYSTEM step renames the
# running exes in Program Files aside and copies the new ones in (the medium-integrity guard cannot
# write there), and --reload restarts the new explorust.exe.
# Usage: tools/g1-deploy.sh
set -eu
cd "$(dirname "${BASH_SOURCE[0]}")/.."
pf='C:\Program Files\explorust'
# The mode query: a failure stops the deploy with what os win said (set -e alone exits silently here).
q=$(timeout 90 os win run g1 --ps -- "if (Test-Path '$pf\\explorust.exe') { 'MODE=installed' } else { 'MODE=plain' }" 2>&1) || { echo "g1-deploy: mode query failed (os win run exit $?): $q" >&2; exit 1; }
qlog=$(echo "$q" | grep '^log:' | awk '{print $2}' || true)
[ -n "$qlog" ] && [ -f "$qlog" ] || { echo "g1-deploy: mode query gave no log file: $q" >&2; exit 1; }
mode=$(grep -o 'MODE=[a-z]*' "$qlog" | cut -d= -f2 || true)
case "$mode" in installed|plain) ;; *) echo "g1-deploy: mode query answered neither installed nor plain: $(head -c 400 "$qlog")" >&2; exit 1 ;; esac
echo "g1 mode: $mode"
ua=0
[ "$mode" = installed ] && ua=1
# Plain cargo: it takes its tokens from the box's shared jobserver (MAKEFLAGS), so it stays fair
# to other threads; os job admission can sit queued behind another thread's long job.
# A rustc the box's memory guard kills (the windows crate is the big one) gets one retry on two
# jobs; a build that still fails stops the deploy instead of pushing the previous binaries.
build() { nice -n 10 env EXPLORUST_UIACCESS=$ua cargo build "$@" --release --target x86_64-pc-windows-gnu -p shell -p keys -p guard -p devkeys -p files -p inject -p notify -p viewer -p taskman -p fsindex; }
build --jobs 3 || build --jobs 2 || { echo "g1-deploy: build failed" >&2; exit 1; }
t=target/x86_64-pc-windows-gnu/release
mkdir -p dist
cp "$t/explorust.exe" "$t/explorust-guard.exe" "$t/keys.exe" "$t/explorust-keys.exe" "$t/explorust-open.exe" "$t/explorust_inject.dll" "$t/explorust-notify.exe" "$t/explorust-viewer.exe" "$t/explorust-taskman.exe" "$t/explorust-index.exe" tools/g1-state.ps1 dist/
python3 -I crates/notify/package/make-msix.py dist/explorust-notify.msix
[ $ua = 1 ] && python3 -I tools/authenticode-sign.py sign dist/explorust-keys.exe
# The index service runs as LocalSystem from Program Files: it needs no signature (no uiAccess).
cp dist/explorust.exe dist/explorust.next.exe
timeout 120 os win push g1 dist/explorust-open.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust.next.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/keys.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust-keys.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust_inject.dll 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/g1-state.ps1 'C:\explorust' >/dev/null
# The helper: set the running one aside, then push it and its package.
timeout 90 os win run g1 --ps -- 'Set-Location C:\explorust; Remove-Item explorust-notify.old*.exe -EA SilentlyContinue; if (Test-Path explorust-notify.exe) { Rename-Item explorust-notify.exe ("explorust-notify.old" + (Get-Random) + ".exe") }; "helper set aside"' >/dev/null
timeout 120 os win push g1 dist/explorust-notify.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust-notify.msix 'C:\explorust' >/dev/null
# The default handlers (image viewer, task manager): a running one locks its exe, so it is renamed aside first.
timeout 90 os win run g1 --ps -- 'Set-Location C:\explorust; foreach ($n in "explorust-viewer", "explorust-taskman") { Remove-Item "$n.old*.exe" -EA SilentlyContinue; if (Test-Path "$n.exe") { Rename-Item "$n.exe" ("$n.old" + (Get-Random) + ".exe") } }; "handlers set aside"' >/dev/null
timeout 120 os win push g1 dist/explorust-viewer.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust-taskman.exe 'C:\explorust' >/dev/null
if [ $ua = 1 ]; then
  # Swap as SYSTEM: <name>.exe -> <name>.old (a running image can be renamed), then copy the new one.
  timeout 90 os win run g1 --ps -- "\$d='$pf'; function swap(\$src, \$name) { if (-not (Test-Path \$src)) { return }; Remove-Item \"\$d\\\$name.old\" -EA SilentlyContinue; if (Test-Path \"\$d\\\$name.exe\") { Rename-Item \"\$d\\\$name.exe\" \"\$name.old\" }; Copy-Item \$src \"\$d\\\$name.exe\"; Remove-Item \$src; \"\$name swapped\" }; Copy-Item C:\\explorust\\explorust-open.exe C:\\explorust\\explorust-open.stage.exe; Copy-Item C:\\explorust\\explorust-keys.exe C:\\explorust\\explorust-keys.stage.exe; swap C:\\explorust\\explorust.next.exe explorust; swap C:\\explorust\\explorust-open.stage.exe explorust-open; swap C:\\explorust\\explorust-keys.stage.exe explorust-keys; Get-ChildItem \"\$d\\explorust-notify.old*.exe\" -EA SilentlyContinue | Remove-Item -EA SilentlyContinue; if (Test-Path \"\$d\\explorust-notify.exe\") { Rename-Item \"\$d\\explorust-notify.exe\" (\"explorust-notify.old\" + (Get-Random) + \".exe\") }; Copy-Item C:\\explorust\\explorust-notify.exe \"\$d\\explorust-notify.exe\"; Copy-Item C:\\explorust\\explorust-notify.msix \"\$d\\explorust-notify.msix\" -Force; 'explorust-notify swapped'; \$dll=\"\$d\\explorust_inject.dll\"; if (Test-Path C:\\explorust\\explorust_inject.dll) { Remove-Item \"\$dll.old\" -EA SilentlyContinue; if (Test-Path \$dll) { Rename-Item \$dll explorust_inject.dll.old }; Copy-Item C:\\explorust\\explorust_inject.dll \$dll; 'explorust_inject swapped' }" >/dev/null
fi
if [ $ua = 1 ]; then
  # The handlers next to explorust.exe in Program Files, as SYSTEM, the running ones renamed aside.
  timeout 90 os win run g1 --ps -- "\$d='$pf'; foreach (\$n in 'explorust-viewer', 'explorust-taskman') { Get-ChildItem \"\$d\\\$n.old*.exe\" -EA SilentlyContinue | Remove-Item -EA SilentlyContinue; if (Test-Path \"\$d\\\$n.exe\") { Rename-Item \"\$d\\\$n.exe\" (\$n + '.old' + (Get-Random) + '.exe') }; Copy-Item \"C:\\explorust\\\$n.exe\" \"\$d\\\$n.exe\"; \"\$n swapped\" }" >/dev/null
  hd=$pf
else
  hd='C:\explorust'
fi
# The index service (explorust-index, LocalSystem): stop it, rename the exe aside, copy the new one
# and run its --install (updates the service config and failure actions, starts it). Installed mode
# only: plain mode has no service.
if [ $ua = 1 ]; then
  timeout 120 os win push g1 dist/explorust-index.exe 'C:\explorust' >/dev/null
  timeout 90 os win run g1 --ps -- "\$d='$pf'; sc.exe stop explorust-index | Out-Null; \$n=0; while ((Get-Process explorust-index -EA SilentlyContinue) -and \$n -lt 50) { Start-Sleep -Milliseconds 100; \$n++ }; Get-ChildItem \"\$d\\explorust-index.old*.exe\" -EA SilentlyContinue | Remove-Item -EA SilentlyContinue; if (Test-Path \"\$d\\explorust-index.exe\") { Rename-Item \"\$d\\explorust-index.exe\" ('explorust-index.old' + (Get-Random) + '.exe') }; Copy-Item C:\\explorust\\explorust-index.exe \"\$d\\explorust-index.exe\"; Remove-Item C:\\explorust\\explorust-index.exe; & \"\$d\\explorust-index.exe\" --install" >/dev/null
fi
# Adopt the on-disk guard before restarting the shell. A replaced file alone leaves the
# old guard's startup policy in charge. All process changes here are confined to g1.
cp dist/explorust-guard.exe dist/explorust-guard.next.exe
timeout 90 os win push g1 dist/explorust-guard.next.exe 'C:\explorust' >/dev/null
guard_command=$(cat <<'PS'
$ErrorActionPreference = 'Stop'
$source = 'C:\explorust\explorust-guard.next.exe'
$live = Join-Path $d 'explorust-guard.exe'
$shell = Get-CimInstance Win32_Process -Filter "Name = 'explorust.exe'" |
    Where-Object { $_.SessionId -gt 0 -and $_.CommandLine -like '* --ready-event *' } |
    Select-Object -First 1
if (-not $shell) { throw 'No running explorust shell; guard adoption requires one.' }
$session = $shell.SessionId
$shellProcess = Get-Process -Id $shell.ProcessId
$old = @(Get-Process explorust-guard | Where-Object SessionId -eq $session)
if ($old.Count -ne 1) { throw "Expected one running guard in session $session, got $($old.Count)." }
$changed = -not (Test-Path $live) -or (Get-FileHash $source).Hash -ne (Get-FileHash $live).Hash
if ($changed) {
    $staged = "$live.stage"
    $aside = "$live.old$($old[0].Id)"
    Copy-Item $source $staged -Force
    if (Test-Path $live) { Move-Item $live $aside -Force }
    try { Move-Item $staged $live }
    catch {
        if (Test-Path $aside) { Move-Item $aside $live }
        throw
    }
}
Remove-Item $source
# The disk hash cannot identify the running image: setup may already have replaced it.
$oldId = $old[0].Id
Stop-Process -Id $oldId -Force
for ($n = 0; $n -lt 100; $n++) {
    $new = Get-Process explorust-guard -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -eq $session -and $_.Id -ne $oldId -and $_.Path -eq $live }
    if ($new -and -not $shellProcess.HasExited) {
        "guard adopted shell $($shellProcess.Id): $oldId -> $($new.Id)"
        break
    }
    Start-Sleep -Milliseconds 100
}
if (-not $new -or $shellProcess.HasExited) { throw 'Replacement guard did not adopt the running shell.' }
"SHELL_PID=$($shellProcess.Id)"
PS
)
guard_result=$(timeout 90 os win run g1 --ps -- "\$d='$hd'; $guard_command" 2>&1) || {
  echo "g1-deploy: guard adoption failed: $guard_result" >&2
  guard_log=$(echo "$guard_result" | awk '/^log: / {print $2}')
  [ -f "$guard_log" ] && cat "$guard_log" >&2
  exit 1
}
guard_log=$(echo "$guard_result" | awk '/^log: / {print $2}')
cat "$guard_log"
old_shell=$(sed -n 's/^SHELL_PID=\([0-9]*\).*/\1/p' "$guard_log")
[ -n "$old_shell" ] || { echo "g1-deploy: guard did not confirm the running shell PID" >&2; exit 1; }
# Register the viewer and request reload in the interactive user's session. In g1 the
# guest's --desktop command can take over a minute after the task itself has run.
reload="$hd\explorust.exe"
[ "$mode" = plain ] && reload="$hd\explorust.next.exe"
reload_command=$(cat <<'PS'
$ErrorActionPreference = 'Stop'
$session = (Get-Process -Id $oldPid).SessionId
$task = "explorust-g1-deploy-$oldPid"
$viewer = New-ScheduledTaskAction -Execute "$d\explorust-viewer.exe" -Argument '--register'
$restart = New-ScheduledTaskAction -Execute $reload -Argument '--reload'
$principal = New-ScheduledTaskPrincipal -UserId "$env:COMPUTERNAME\agent" -LogonType Interactive
Register-ScheduledTask -TaskName $task -Action @($viewer, $restart) -Principal $principal -Force | Out-Null
try {
    Start-ScheduledTask -TaskName $task
    for ($n = 0; $n -lt 100; $n++) {
        if (-not (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) {
            $new = Get-CimInstance Win32_Process -Filter "Name = 'explorust.exe' AND SessionId = $session" |
                Where-Object CommandLine -like '* --ready-event *'
            if ($new) { break }
        }
        Start-Sleep -Milliseconds 100
    }
    if ($n -eq 100) { throw 'The shell did not restart under the guard.' }
    & C:\explorust\g1-state.ps1 -Tail 8
} finally {
    Unregister-ScheduledTask -TaskName $task -Confirm:$false
}
PS
)
reload_status=0
out=$(timeout 90 os win run g1 --ps -- "\$d='$hd'; \$reload='$reload'; \$oldPid=$old_shell; $reload_command" 2>&1) || reload_status=$?
reload_log=$(echo "$out" | awk '/^log: / {print $2}')
if [ -f "$reload_log" ]; then cat "$reload_log"; else echo "$out"; fi
exit "$reload_status"
