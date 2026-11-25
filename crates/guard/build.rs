//! The Winlogon entry and maintenance commands keep a plain asInvoker manifest.

#[path = "../msix-build.rs"]
mod msix_build;

fn main() {
    if std::env::var("CARGO_PKG_NAME").as_deref() == Ok("explorust-switch") {
        msix_build::resource("explorust-switch", "../guard/res/app.manifest", None, false);
    } else {
        msix_build::resource("explorust-guard", "res/app.manifest", None, false);
        msix_build::resource("explorust-supervisor", "res/app.manifest", Some("guard"), false);
    }
}
