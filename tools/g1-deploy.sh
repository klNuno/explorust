#!/usr/bin/env bash
# Builds explorust (with every part) and the guard for Windows, sends them to the guest g1 and
# reloads the running shell: explorust exits with code 5, the guard swaps in explorust.next.exe
# and starts it. --guard also replaces explorust-guard.exe (renamed while it runs, used from the
# next logon). Run it under tools/g1-locked.sh when other workers share the guest.
# Usage: tools/g1-deploy.sh [--guard]
set -eu
cd /home/boite/projects/explorust
os job run --wait -- cargo build --release --target x86_64-pc-windows-gnu -p shell -p guard -p devkeys -p files
t=target/x86_64-pc-windows-gnu/release
mkdir -p dist
cp "$t/explorust.exe" dist/explorust.next.exe
cp "$t/explorust.exe" "$t/explorust-guard.exe" "$t/keys.exe" "$t/explorust-open.exe" tools/g1-state.ps1 dist/
timeout 120 os win push g1 dist/explorust-open.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/explorust.next.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/keys.exe 'C:\explorust' >/dev/null
timeout 120 os win push g1 dist/g1-state.ps1 'C:\explorust' >/dev/null
if [ "${1:-}" = "--guard" ]; then
  cp "$t/explorust-guard.exe" dist/explorust-guard.next.exe
  timeout 120 os win push g1 dist/explorust-guard.next.exe 'C:\explorust' >/dev/null
  timeout 90 os win run g1 --ps -- 'Set-Location C:\explorust; Remove-Item explorust-guard.old.exe -EA SilentlyContinue; if (Test-Path explorust-guard.exe) { Rename-Item explorust-guard.exe explorust-guard.old.exe }; Rename-Item explorust-guard.next.exe explorust-guard.exe; "guard replaced"' >/dev/null
fi
timeout 90 os win run g1 --desktop -- 'C:\explorust\explorust.exe' --reload >/dev/null 2>&1 || true
sleep 4
out=$(timeout 90 os win run g1 --ps -- '& C:\explorust\g1-state.ps1 -Tail 8' 2>&1)
cat "$(echo "$out" | grep '^log:' | awk '{print $2}')"
