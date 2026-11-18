#!/usr/bin/env bash
# Reboots the guest g1, waits for a shell (explorust-guard or explorer) in the user session, waits
# $1 more seconds (default 60) for the guard to settle, then prints the explorust state.
# Run from the project's BoiteOS thread: tools/g1-reboot.sh [settle_seconds]
set -u
settle=${1:-60}
os win run g1 -- shutdown /r /t 0 >/dev/null 2>&1
sleep 45
for i in $(seq 1 40); do
  if os win run g1 --ps -- 'if (Get-Process explorust-guard,explorer -EA SilentlyContinue) { "SHELLUP" }' 2>&1 | grep -q SHELLUP; then
    echo "shell process up after $((45 + (i - 1) * 20)) s or so"
    break
  fi
  sleep 20
done
sleep "$settle"
out=$(os win run g1 --ps -- '& C:\explorust\g1-state.ps1 -Tail 14' 2>&1)
cat "$(echo "$out" | grep '^log:' | awk '{print $2}')"
