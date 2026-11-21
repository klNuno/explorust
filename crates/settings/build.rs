use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=res/app.rc");
    println!("cargo:rerun-if-changed=res/explorust.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("settings-icon.o");
    let windres = env::var_os("WINDRES").unwrap_or_else(|| "x86_64-w64-mingw32-windres".into());
    let status = Command::new(windres)
        .current_dir("res")
        .args(["-i", "app.rc", "-O", "coff", "-o"])
        .arg(&out)
        .status()
        .expect("run windres");
    assert!(status.success(), "could not embed the settings icon");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
