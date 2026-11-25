//! Windows resource compilation for the path-launched members of the sparse package.

use std::{env, fs, path::PathBuf, process::Command};

pub fn resource(binary: &str, source: &str, application: Option<&str>, ui_access: bool) {
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-env-changed=EXPLORUST_PACKAGE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut manifest = fs::read_to_string(source).expect("read executable manifest");
    if let Some(application) = application
        && env::var("EXPLORUST_PACKAGE").as_deref() == Ok("1")
    {
        let binding = format!(
            r#"<msix xmlns="urn:schemas-microsoft-com:msix.v1" publisher="CN=explorust local code signing" packageName="explorust.shell" applicationId="{application}"/>"#,
        );
        manifest = manifest.replace("</assembly>", &format!("  {binding}\n</assembly>"));
        println!("cargo:rustc-env=EXPLORUST_PACKAGE_BUILD=1");
    }
    if ui_access {
        assert!(manifest.contains(r#"uiAccess="false""#));
        manifest = manifest.replace(r#"uiAccess="false""#, r#"uiAccess="true""#);
    }
    let manifest_path = out.join(format!("{binary}.manifest"));
    let resource_path = out.join(format!("{binary}.rc"));
    let obj = out.join(format!("{binary}.o"));
    fs::write(manifest_path, manifest).expect("write executable manifest");
    fs::write(&resource_path, format!("1 24 \"{binary}.manifest\"\n")).expect("write resource");
    let windres = env::var_os("WINDRES").unwrap_or_else(|| "x86_64-w64-mingw32-windres".into());
    let status = Command::new(windres)
        .current_dir(out)
        .args(["-i", &format!("{binary}.rc"), "-O", "coff", "-o"])
        .arg(&obj)
        .status()
        .expect("start windres");
    assert!(status.success(), "could not embed the {binary} manifest");
    println!("cargo:rustc-link-arg-bin={binary}={}", obj.display());
}
