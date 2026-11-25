//! The release keys helper uses UIAccess and the same identity as its shell parent.

#[path = "../msix-build.rs"]
mod msix_build;

fn main() {
    println!("cargo:rerun-if-env-changed=EXPLORUST_UIACCESS");
    let ui_access = std::env::var("EXPLORUST_UIACCESS").as_deref() == Ok("1");
    msix_build::resource("explorust-keys", "res/app.manifest", Some("keys"), ui_access);
}
