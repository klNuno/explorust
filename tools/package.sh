#!/usr/bin/env bash
# Builds the release installer: dist/explorust-setup.exe, one file a user double-clicks.
# Builds and signs every PE in the payload and the installer. The keyboard hook uses uiAccess.
# Dev leftovers in dist are never shipped: the payload folder is filled from target/ only.
# MSIX packing defaults to MakeAppx in g1. EXPLORUST_MAKEAPPX can point to a wrapper
# taking the staging directory and output path. OSSLSIGNCODE can override the local signer.
# Usage: tools/package.sh
set -eu
cd -- "$(dirname -- "$0")/.."
t=target/x86_64-pc-windows-gnu/release
pay=target/package/payload
build() { nice -n 10 env EXPLORUST_PACKAGE=1 EXPLORUST_UIACCESS=1 cargo build --release --locked --target x86_64-pc-windows-gnu "$@"; }
build --jobs 3 -p shell -p keys -p guard -p explorust-switch -p files -p inject -p notify -p viewer -p taskman -p fsindex -p settings
rm -rf target/package
mkdir -p "$pay" dist
for f in explorust.exe explorust-guard.exe explorust-supervisor.exe explorust-switch.exe explorust-keys.exe explorust-open.exe explorust_inject.dll explorust-notify.exe explorust-viewer.exe explorust-taskman.exe explorust-index.exe explorust-settings.exe; do
  cp "$t/$f" "$pay/$f"
done
python3 -I crates/notify/package/make-msix.py target/package/msix --shell
"${EXPLORUST_MAKEAPPX:-tools/g1-makeappx.sh}" target/package/msix target/package/unsigned.msix
python3 -I tools/authenticode-sign.py sign-msix target/package/unsigned.msix "$pay/explorust-shell.msix"
python3 -I tools/authenticode-sign.py sign "$pay"/*.exe "$pay"/*.dll
# The installer embeds the payload at compile time; a changed file reruns its build script.
EXPLORUST_PAYLOAD="$PWD/$pay" build --jobs 3 -p setup
cp "$t/explorust-setup.exe" dist/explorust-setup.exe
python3 -I tools/authenticode-sign.py sign dist/explorust-setup.exe
ls -l "$pay" dist/explorust-setup.exe
sha256sum dist/explorust-setup.exe
