#!/usr/bin/env bash
# Builds explorust (with every part) and the guard for Windows, sends them to the guest g1 and
# reloads the running shell: explorust exits with code 5, the guard swaps in explorust.next.exe
# and starts it. --guard also replaces explorust-guard.exe (renamed while it runs, used from the
# next logon). Run it under tools/g1-locked.sh when other workers share the guest.
#
# Installed mode (`explorust-guard --install` run elevated, the shell in C:\Program Files\explorust):
# explorust.exe is built with uiAccess (EXPLORUST_UIACCESS=1) and signed with
# tools/authenticode-sign.py; the files go to C:\explorust as before, then a SYSTEM step renames the
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
os job run --wait -- env EXPLORUST_UIACCESS=$ua cargo build --release --target x86_64-pc-windows-gnu -p shell -p guard -p devkeys -p files
t=target/x86_64-pc-windows-gnu/release
mkdir -p dist
cp "$t/explorust.exe" "$t/explorust-guard.exe" "$t/keys.exe" "$t/explorust-open.exe" tools/g1-state.ps1 dist/
[ $ua = 1 ] && python3 -I tools/authenticode-sign.py sign dist/explorust.exe
cp dist/explorust.exe dist/explorust.next.exe
timeout 120 os win push g1 dist/explorust-open.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust.next.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/keys.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/g1-state.ps1 'C:\explorust' >/dev/null
if [ "${1:-}" = "--guard" ]; then
  cp "$t/explorust-guard.exe" dist/explorust-guard.next.exe
  timeout 120 os win push g1 dist/explorust-guard.next.exe 'C:\explorust' >/dev/null
  if [ $ua = 0 ]; then
    timeout 90 os win run g1 --ps -- 'Set-Location C:\explorust; Remove-Item explorust-guard.old.exe -EA SilentlyContinue; if (Test-Path explorust-guard.exe) { Rename-Item explorust-guard.exe explorust-guard.old.exe }; Rename-Item explorust-guard.next.exe explorust-guard.exe; "guard replaced"' >/dev/null
  fi
fi
if [ $ua = 1 ]; then
  # Swap as SYSTEM: <name>.exe -> <name>.old (a running image can be renamed), then copy the new one.
  timeout 90 os win run g1 --ps -- "\$d='$pf'; function swap(\$src, \$name) { if (-not (Test-Path \$src)) { return }; Remove-Item \"\$d\\\$name.old\" -EA SilentlyContinue; if (Test-Path \"\$d\\\$name.exe\") { Rename-Item \"\$d\\\$name.exe\" \"\$name.old\" }; Copy-Item \$src \"\$d\\\$name.exe\"; Remove-Item \$src; \"\$name swapped\" }; Copy-Item C:\\explorust\\explorust-open.exe C:\\explorust\\explorust-open.stage.exe; swap C:\\explorust\\explorust.next.exe explorust; swap C:\\explorust\\explorust-open.stage.exe explorust-open; swap C:\\explorust\\explorust-guard.next.exe explorust-guard" >/dev/null
fi
# Any explorust.exe can ask the running one to reload; the staging copy is never the uiAccess one.
timeout 90 os win run g1 --desktop -- 'C:\explorust\explorust.exe' --reload >/dev/null 2>&1 || true
sleep 4
out=$(timeout 90 os win run g1 --ps -- '& C:\explorust\g1-state.ps1 -Tail 8' 2>&1)
cat "$(echo "$out" | grep '^log:' | awk '{print $2}')"
