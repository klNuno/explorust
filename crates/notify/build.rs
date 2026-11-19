//! Embeds `res/explorust-notify.manifest` in explorust-notify.exe only: its `msix` element gives
//! that helper the identity of the sparse package in `package/` (see `src/win/helper.rs`).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/explorust-notify.rc");
    println!("cargo:rerun-if-changed=res/explorust-notify.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let res = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for f in ["explorust-notify.manifest", "explorust-notify.rc"] {
        std::fs::copy(res.join(f), out_dir.join(f)).expect("copy a resource file");
    }
    let out = out_dir.join("explorust-notify-manifest.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let ok = Command::new(&windres)
        .current_dir(&out_dir)
        .args(["-i", "explorust-notify.rc", "-O", "coff", "-o"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        println!("cargo:rustc-link-arg-bin=explorust-notify={}", out.display());
    } else {
        println!("cargo:warning=windres failed: explorust-notify.exe built without its package identity");
    }
}
