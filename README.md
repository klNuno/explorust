# explorust

A Windows 11 shell written in Rust that replaces explorer.exe: taskbar, Start and search, Alt+Tab, desktop, File Explorer, context menus, notification cards, an image viewer and a task manager. It aims to look and behave like Windows 11, open everything instantly and stay light, while keeping Windows one step away.

Status: experimental. Tested on Windows 11 builds 26200 and 26300.

## Rollback first

Winlogon starts `explorust-guard.exe`, never explorust itself. The guard brings explorer.exe back when explorust crashes twice, does not get ready in time, hangs, or fails its self-test after a Windows update. At any time:

- Ctrl+Alt+Shift+F12 switches to explorer for the session;
- the explorust tray icon offers "Back to Windows" and "Uninstall";
- `explorust-guard --restore` removes the shell setting, so the next logon is plain Windows.

## Parts

| Crate | Role |
| --- | --- |
| `guard` | Winlogon shell entry, supervision, fallback, install and uninstall |
| `shell` | The explorust process: parts as supervised threads, startup apps |
| `uwp-host` | Hosts twinui's immersive shell so UWP apps and Settings run without explorer (vendored nickel-uwu, MIT OR Apache-2.0) |
| `taskbar`, `start`, `switcher`, `desktop`, `notify` | Shell surfaces |
| `files`, `menu` | File Explorer and context menus on Windows' own shell APIs |
| `keys` | Signed uiAccess helper for global hotkeys over elevated windows |
| `index` | MFT and USN journal file index service for instant search |
| `render` | Direct2D, DirectWrite and DirectComposition drawing, shared styles |
| `settings` | Appearance per surface (Windows, Acrylic, Transparent, Mica, OLED black) |
| `viewer`, `taskman` | Default image viewer and task manager |
| `setup` | One-file installer |

## Build

Cross-built from Linux with the GNU toolchain:

```sh
cargo build --release --target x86_64-pc-windows-gnu
```

`tools/package.sh` builds the signed installer `dist/explorust-setup.exe`. It needs your own code-signing key: `explorust-keys.exe` runs with uiAccess, which Windows only allows for a signed exe in Program Files.
