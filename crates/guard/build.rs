//! The Winlogon entry and maintenance commands keep a plain asInvoker manifest.

#[path = "../msix-build.rs"]
mod msix_build;

fn main() {
    if std::env::var("CARGO_PKG_NAME").as_deref() == Ok("explorust-switch") {
        msix_build::resource("explorust-switch", "../guard/res/app.manifest", None, false);
    } else {
        println!("cargo:rerun-if-changed=../notify/package/ShellAppxManifest.xml");
        let manifest = include_str!("../notify/package/ShellAppxManifest.xml");
        let version = manifest.lines().find(|line| line.trim_start().starts_with("<Identity "))
            .and_then(|line| line.split_once(" Version=\""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .expect("shell package identity version").0;
        println!("cargo:rustc-env=EXPLORUST_MSIX_VERSION={version}");
        msix_build::resource("explorust-guard", "res/app.manifest", None, false);
        msix_build::resource("explorust-supervisor", "res/app.manifest", Some("guard"), false);
    }
}
