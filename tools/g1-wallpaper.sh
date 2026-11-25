#!/usr/bin/env bash
# Run under tools/g1-locked.sh from the checkout with a built desktop-lively.exe.
# Usage: tools/g1-wallpaper.sh <capture-name> <workerw|progman> [flip|composition|gdi]
set -euo pipefail

name=$1
parent=$2
render=${3:-flip}
color=23D46F
guest_dir='C:\explorust\wallpaper-probe'
task='explorust-wallpaper-probe'
capture="$PWD/captures/$name.png"
exe='target/x86_64-pc-windows-gnu/release/desktop-lively.exe'

case "$parent/$render" in
    workerw/flip|progman/flip|workerw/composition|progman/composition|workerw/gdi|progman/gdi) ;;
    *) echo "Invalid wallpaper probe mode: $parent/$render" >&2; exit 2 ;;
esac

guest() { timeout 90 os win run g1 --ps -- "$1"; }
cleanup() {
    guest "
        Stop-ScheduledTask -TaskName '$task' -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName '$task' -Confirm:\$false -ErrorAction SilentlyContinue
        Remove-Item '$guest_dir' -Recurse -Force -ErrorAction SilentlyContinue
    " >/dev/null || true
}
trap cleanup EXIT
mkdir -p "$(dirname "$capture")"
guest "New-Item -ItemType Directory -Force '$guest_dir' | Out-Null" >/dev/null
timeout 90 os win push g1 "$exe" "$guest_dir" >/dev/null
guest "
    \$ErrorActionPreference = 'Stop'
    Remove-Item '$guest_dir\desktop-lively.log' -ErrorAction SilentlyContinue
    \$action = New-ScheduledTaskAction -Execute '$guest_dir\desktop-lively.exe' -Argument '$parent 60 $color $render'
    \$principal = New-ScheduledTaskPrincipal -UserId \"\$env:COMPUTERNAME\agent\" -LogonType Interactive
    Register-ScheduledTask -TaskName '$task' -Action \$action -Principal \$principal -Force | Out-Null
    Start-ScheduledTask -TaskName '$task'
" >/dev/null

status=0
out=$(guest "
    \$path = '$guest_dir\desktop-lively.log'
    for (\$i = 0; \$i -lt 60 -and -not (Test-Path \$path); \$i++) { Start-Sleep -Milliseconds 250 }
    if (-not (Test-Path \$path)) {
        Get-ScheduledTaskInfo '$task' | Format-List LastRunTime,LastTaskResult
        exit 2
    }
    Get-Content \$path
") || status=$?
log=$(awk '/^log: / {print $2}' <<< "$out")
if [[ ! -f "$log" ]]; then
    echo "$out" >&2
    exit 2
fi
cat "$log"
if [[ $status != 0 ]] || rg -q '^FAIL:' "$log" || ! rg -q 'parented to' "$log"; then
    echo "ERROR: wallpaper probe did not attach" >&2
    exit 2
fi

timeout 60 os win shot g1 --out "$capture"
python3 - "$capture" "$color" "$render" "$log" <<'PY'
from pathlib import Path
import re
import struct
import subprocess
import sys

capture, expected, render, log = sys.argv[1:]
with open(capture, "rb") as png:
    width, height = struct.unpack(">II", png.read(24)[16:24])
x, y = width // 2, height // 2
if render != "gdi":
    match = re.search(rf"{render} present 0x[0-9a-f]+; expect #[0-9A-F]+ at (\d+),(\d+)", Path(log).read_text())
    if not match or tuple(map(int, match.groups())) != (x, y):
        sys.exit("ERROR: missing GPU presentation or screenshot does not match the primary display")
else:
    y -= 12  # The GDI fixture writes text across the middle of its window.
pixel = subprocess.check_output([
    "ffmpeg", "-v", "error", "-threads", "1", "-i", capture, "-filter_threads", "1",
    "-vf", f"format=rgb24,crop=1:1:{x}:{y}:exact=1", "-frames:v", "1",
    "-pix_fmt", "rgb24", "-c:v", "rawvideo", "-threads", "1", "-f", "rawvideo", "-",
])
actual = pixel.hex().upper()
visible = actual == expected
print(f"{'PASS' if visible else 'FAIL'} {render} pixel ({x},{y}): #{actual}, expected #{expected}")
sys.exit(0 if visible else 1)
PY
