#!/usr/bin/env bash
# Builds explorust (with every part) and the guard for Windows, sends them to the guest g1 and
# reloads the running shell: explorust exits with code 5, the guard swaps in explorust.next.exe
# and starts it. --guard also replaces explorust-guard.exe (renamed while it runs, used from the
# next logon). Run it under tools/g1-locked.sh when other workers share the guest.
# The notify helper (explorust-notify.exe, run with the identity of its sparse package) and that
# package (explorust-notify.msix, registered by --install) go along: the running helper locks its
# exe, so it is renamed aside (explorust-notify.old<n>.exe, deleted by a later deploy) before the copy.
#
# Installed mode (`explorust-guard --install` run elevated, the shell in C:\Program Files\explorust):
# explorust-keys.exe (the keyboard hook) is built with uiAccess (EXPLORUST_UIACCESS=1) and signed with
# tools/authenticode-sign.py, explorust.exe stays plain; the files go to C:\explorust as before, then a SYSTEM step renames the
# running exes in Program Files aside and copies the new ones in (the medium-integrity guard cannot
# write there), and --reload restarts the new explorust.exe.
# Usage: tools/g1-deploy.sh [--guard]
set -eu
cd /home/boite/projects/explorust
pf='C:\Program Files\explorust'
mode=$(timeout 90 os win run g1 --ps -- "if (Test-Path '$pf\\explorust.exe') { 'MODE=installed' } else { 'MODE=plain' }" 2>&1)
mode=$(cat "$(echo "$mode" | grep '^log:' | awk '{print $2}')" | grep -o 'MODE=[a-z]*' | cut -d= -f2)
echo "g1 mode: ${mode:-unknown}"
ua=0
[ "$mode" = installed ] && ua=1
os job run --wait -- env EXPLORUST_UIACCESS=$ua cargo build --release --target x86_64-pc-windows-gnu -p shell -p keys -p guard -p devkeys -p files -p inject -p notify -p viewer -p taskman
t=target/x86_64-pc-windows-gnu/release
mkdir -p dist
cp "$t/explorust.exe" "$t/explorust-guard.exe" "$t/keys.exe" "$t/explorust-keys.exe" "$t/explorust-open.exe" "$t/explorust_inject.dll" "$t/explorust-notify.exe" "$t/explorust-viewer.exe" "$t/explorust-taskman.exe" tools/g1-state.ps1 dist/
python3 -I crates/notify/package/make-msix.py dist/explorust-notify.msix
[ $ua = 1 ] && python3 -I tools/authenticode-sign.py sign dist/explorust-keys.exe
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
if [ "${1:-}" = "--guard" ]; then
  cp "$t/explorust-guard.exe" dist/explorust-guard.next.exe
  timeout 120 os win push g1 dist/explorust-guard.next.exe 'C:\explorust' >/dev/null
  if [ $ua = 0 ]; then
    timeout 90 os win run g1 --ps -- 'Set-Location C:\explorust; Remove-Item explorust-guard.old.exe -EA SilentlyContinue; if (Test-Path explorust-guard.exe) { Rename-Item explorust-guard.exe explorust-guard.old.exe }; Rename-Item explorust-guard.next.exe explorust-guard.exe; "guard replaced"' >/dev/null
  fi
fi
if [ $ua = 1 ]; then
  # Swap as SYSTEM: <name>.exe -> <name>.old (a running image can be renamed), then copy the new one.
  timeout 90 os win run g1 --ps -- "\$d='$pf'; function swap(\$src, \$name) { if (-not (Test-Path \$src)) { return }; Remove-Item \"\$d\\\$name.old\" -EA SilentlyContinue; if (Test-Path \"\$d\\\$name.exe\") { Rename-Item \"\$d\\\$name.exe\" \"\$name.old\" }; Copy-Item \$src \"\$d\\\$name.exe\"; Remove-Item \$src; \"\$name swapped\" }; Copy-Item C:\\explorust\\explorust-open.exe C:\\explorust\\explorust-open.stage.exe; Copy-Item C:\\explorust\\explorust-keys.exe C:\\explorust\\explorust-keys.stage.exe; swap C:\\explorust\\explorust.next.exe explorust; swap C:\\explorust\\explorust-open.stage.exe explorust-open; swap C:\\explorust\\explorust-keys.stage.exe explorust-keys; swap C:\\explorust\\explorust-guard.next.exe explorust-guard; Get-ChildItem \"\$d\\explorust-notify.old*.exe\" -EA SilentlyContinue | Remove-Item -EA SilentlyContinue; if (Test-Path \"\$d\\explorust-notify.exe\") { Rename-Item \"\$d\\explorust-notify.exe\" (\"explorust-notify.old\" + (Get-Random) + \".exe\") }; Copy-Item C:\\explorust\\explorust-notify.exe \"\$d\\explorust-notify.exe\"; Copy-Item C:\\explorust\\explorust-notify.msix \"\$d\\explorust-notify.msix\" -Force; 'explorust-notify swapped'; \$dll=\"\$d\\explorust_inject.dll\"; if (Test-Path C:\\explorust\\explorust_inject.dll) { Remove-Item \"\$dll.old\" -EA SilentlyContinue; if (Test-Path \$dll) { Rename-Item \$dll explorust_inject.dll.old }; Copy-Item C:\\explorust\\explorust_inject.dll \$dll; 'explorust_inject swapped' }" >/dev/null
fi
if [ $ua = 1 ]; then
  # The handlers next to explorust.exe in Program Files, as SYSTEM, the running ones renamed aside.
  timeout 90 os win run g1 --ps -- "\$d='$pf'; foreach (\$n in 'explorust-viewer', 'explorust-taskman') { Get-ChildItem \"\$d\\\$n.old*.exe\" -EA SilentlyContinue | Remove-Item -EA SilentlyContinue; if (Test-Path \"\$d\\\$n.exe\") { Rename-Item \"\$d\\\$n.exe\" (\$n + '.old' + (Get-Random) + '.exe') }; Copy-Item \"C:\\explorust\\\$n.exe\" \"\$d\\\$n.exe\"; \"\$n swapped\" }" >/dev/null
  hd=$pf
else
  hd='C:\explorust'
fi
# The viewer registers itself for the logged-on user (--ps runs as SYSTEM): ProgID, OpenWithProgids,
# RegisteredApplications under HKCU; no default changes.
timeout 90 os win run g1 --desktop -- "$hd\explorust-viewer.exe" --register >/dev/null 2>&1 || true
# Any explorust.exe can ask the running one to reload.
timeout 90 os win run g1 --desktop -- 'C:\explorust\explorust.exe' --reload >/dev/null 2>&1 || true
sleep 4
out=$(timeout 90 os win run g1 --ps -- '& C:\explorust\g1-state.ps1 -Tail 8' 2>&1)
cat "$(echo "$out" | grep '^log:' | awk '{print $2}')"
