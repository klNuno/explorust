//! Embeds `res/app.manifest` (supported OS: Windows 10 and 11, common controls 6) in explorust.exe:
//! without it Windows treats it as a Windows 7 program and refuses WS_EX_LAYERED on child windows,
//! which the desktop part needs to sit under wallpaper apps.
//!
//! `EXPLORUST_UIACCESS=1` at build time sets `uiAccess="true"`: the keyboard hook then sees keys
//! while an elevated window is in front, but Windows only starts that exe when it is signed by a
//! trusted certificate (`tools/authenticode-sign.py`) and lives in Program Files
//! (`explorust-guard --install`). The default build keeps `uiAccess="false"` and runs from anywhere.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-env-changed=EXPLORUST_UIACCESS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let res = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ui_access = std::env::var("EXPLORUST_UIACCESS").as_deref() == Ok("1");
    let manifest = std::fs::read_to_string(res.join("app.manifest")).expect("res/app.manifest");
    let manifest = if ui_access {
        assert!(manifest.contains(r#"uiAccess="false""#), "app.manifest lost its uiAccess attribute");
        manifest.replace(r#"uiAccess="false""#, r#"uiAccess="true""#)
    } else {
        manifest
    };
    std::fs::write(out_dir.join("app.manifest"), manifest).expect("write the manifest");
    std::fs::copy(res.join("app.rc"), out_dir.join("app.rc")).expect("copy app.rc");
    let out = out_dir.join("app-manifest.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "x86_64-w64-mingw32-windres".into());
    let ok = Command::new(&windres)
        .current_dir(&out_dir)
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
