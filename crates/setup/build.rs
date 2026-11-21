//! Embeds the manifest (the installer asks Windows for administrator rights at launch) and the
//! payload: every file of `EXPLORUST_PAYLOAD` (a folder `tools/package.sh` fills with the release
//! build) becomes a byte array in `payload.rs`. Without that variable the payload is empty and the
//! installer says so.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/app.manifest");
    println!("cargo:rerun-if-env-changed=EXPLORUST_PAYLOAD");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut list = String::from("pub const FILES: &[(&str, &[u8])] = &[\n");
    if let Ok(dir) = std::env::var("EXPLORUST_PAYLOAD") {
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("EXPLORUST_PAYLOAD is not a folder")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        names.sort();
        for p in names {
            println!("cargo:rerun-if-changed={}", p.display());
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            list.push_str(&format!("    ({name:?}, include_bytes!({:?})),\n", p.canonicalize().unwrap()));
        }
    }
    list.push_str("];\n");
    std::fs::write(out_dir.join("payload.rs"), list).expect("write payload.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let out = out_dir.join("app-manifest.o");
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
        println!("cargo:warning=windres failed: explorust-setup.exe built without its manifest");
    }
}
