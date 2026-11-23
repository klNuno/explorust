//! Gives the guard and switch native task dialogs, per-monitor DPI and asInvoker privileges.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../guard/res/app.rc");
    println!("cargo:rerun-if-changed=../guard/res/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../guard/res");
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
        println!("cargo:warning=windres failed: built without the task dialog manifest (plain message boxes)");
    }
}
