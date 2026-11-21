#!/usr/bin/env bash
# Builds the release installer: dist/explorust-setup.exe, one file a user double-clicks.
# Release build of every shipped exe, explorust-keys.exe built with uiAccess and signed
# (tools/authenticode-sign.py, key in .agents/keys/), the notification helper's package, then the
# installer with those files embedded (crates/setup, EXPLORUST_PAYLOAD) and signed too.
# Dev leftovers in dist/ are never shipped: the payload folder is filled from target/ only.
# Usage: tools/package.sh
set -eu
cd /home/boite/projects/explorust
t=target/x86_64-pc-windows-gnu/release
pay=target/package/payload
build() { nice -n 10 env EXPLORUST_UIACCESS=1 cargo build "$@" --release --target x86_64-pc-windows-gnu -p shell -p keys -p guard -p files -p inject -p notify -p viewer -p taskman -p fsindex; }
build || build -j 2 || { echo "package: build failed" >&2; exit 1; }
rm -rf target/package
mkdir -p "$pay" dist
for f in explorust.exe explorust-guard.exe explorust-keys.exe explorust-open.exe explorust_inject.dll explorust-notify.exe explorust-viewer.exe explorust-taskman.exe explorust-index.exe; do
  cp "$t/$f" "$pay/$f"
done
python3 -I crates/notify/package/make-msix.py "$pay/explorust-notify.msix"
python3 -I tools/authenticode-sign.py sign "$pay/explorust-keys.exe"
# The installer embeds the payload at compile time; a changed file reruns its build script.
nice -n 10 env EXPLORUST_PAYLOAD="$PWD/$pay" cargo build --release --target x86_64-pc-windows-gnu -p setup
cp "$t/explorust-setup.exe" dist/explorust-setup.exe
python3 -I tools/authenticode-sign.py sign dist/explorust-setup.exe
ls -l "$pay" dist/explorust-setup.exe
sha256sum dist/explorust-setup.exe
