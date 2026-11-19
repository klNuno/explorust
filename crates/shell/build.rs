//! Embeds `res/app.manifest` (supported OS: Windows 10 and 11, common controls 6) in explorust.exe:
//! without it Windows treats it as a Windows 7 program and refuses WS_EX_LAYERED on child windows,
//! which the desktop part needs to sit under wallpaper apps.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app-manifest.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let ok = Command::new(&windres)
        .current_dir(&dir)
        .args(["-i", "app.rc", "-O", "coff", "-o"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bins={}", out.display());
    } else {
        println!("cargo:warning=windres failed: exes built without the Windows 10 manifest");
    }
}
