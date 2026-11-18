# uwp-host

Vendored copy of the `nickel-uwu` crate from [shortontech/nickel](https://github.com/shortontech/nickel),
commit `298f87a`, licensed MIT OR Apache-2.0 (Copyright (c) 2026 Steven Horton). The licence texts
are in `LICENSE-MIT` and `LICENSE-APACHE`, the upstream README in `UPSTREAM-README.md`.

It hosts the immersive shell controller of `twinui.pcshell.dll` so that UWP apps and Settings
present their frames when explorer.exe is not running. Private entry points are resolved from
Microsoft's public PDBs and cached under `%LOCALAPPDATA%\Nickel\symbols`.

Changes from upstream: the package and library are renamed `uwp-host` / `uwp_host`; the source is
otherwise unchanged. Keep local changes small and listed here so the upstream diff stays readable.
