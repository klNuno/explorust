#!/usr/bin/env bash
# Pack the manifest and logos in g1 with Microsoft's Windows SDK. Signing stays on the build host.
# Usage: tools/g1-makeappx.sh <staging-directory> <unsigned.msix>
set -euo pipefail
staging=$(realpath "${1:?MSIX staging directory}")
out=$(realpath -m "${2:?unsigned MSIX path}")
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

sdk=10.0.26100.9457
tools="target/windows-sdk/$sdk"
archive="target/windows-sdk/buildtools-$sdk.nupkg"
files=(makeappx.exe appxpackaging.dll opcservices.dll
  Microsoft.Windows.Build.Appx.AppxPackaging.dll.manifest
  Microsoft.Windows.Build.Appx.OpcServices.dll.manifest)
mkdir -p "$tools"
missing=false
for file in "${files[@]}"; do
  [[ -f "$tools/$file" ]] || missing=true
done
if "$missing"; then
  if [[ ! -f "$archive" ]]; then
    curl -fsSL --retry 2 -o "$archive.part" \
      "https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/$sdk/microsoft.windows.sdk.buildtools.$sdk.nupkg"
    mv "$archive.part" "$archive"
  fi
  printf '%s  %s\n' '4c2e0d85f3490e7f7c065c35b4c47fe913b1bc9627e9711ae349b2cdc7edc791' "$archive" | sha256sum -c -
  unzip -jo "$archive" "${files[@]/#/bin/10.0.26100.0/x64/}" -d "$tools"
fi

scratch=$(mktemp -d "$root/target/explorust-makeappx-XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
mkdir "$scratch/input" "$scratch/output"
cp "${files[@]/#/$tools/}" "$scratch"
cp "$staging"/AppxManifest.xml "$staging"/*.png "$scratch/input"
export EXPLORUST_MSIX_TRANSFER="$scratch"
tools/g1-locked.sh bash -euo pipefail -c '
  scratch=$EXPLORUST_MSIX_TRANSFER
  remote="C:\Windows\Temp\\${scratch##*/}"
  cleanup() { os win run g1 --ps -- "Remove-Item -LiteralPath '\''$remote'\'' -Recurse -Force -ErrorAction SilentlyContinue" >/dev/null; }
  trap cleanup EXIT
  os win push g1 "$scratch" "C:\Windows\Temp"
  # A sparse package declares executables supplied by the external install location.
  os win run g1 -- "$remote\makeappx.exe" pack /d "$remote\input" /p "$remote\unsigned.msix" /nv /o
  os win pull g1 "$remote\unsigned.msix" "$scratch/output"
'
mv "$scratch/output/unsigned.msix" "$out"
