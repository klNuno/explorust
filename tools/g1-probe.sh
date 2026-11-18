#!/usr/bin/env bash
# Presses keys in the guest through keys.exe (no window, foreground untouched), waits, then saves a
# capture as captures/<name>.png. Usage: tools/g1-probe.sh <name> <wait_s> <keys args...>
set -u
name=$1; settle=$2; shift 2
timeout 90 os win run g1 --desktop -- cmd /c start "" 'C:\explorust\keys.exe' wait:1.5 "$@" >/dev/null 2>&1
sleep "$settle"
timeout 60 os win shot g1 --out "/home/boite/projects/explorust/captures/$name.png" >/dev/null 2>&1 && echo "captures/$name.png"
