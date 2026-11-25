//! The desktop shell keeps its Windows compatibility manifest, and has no UIAccess.

#[path = "../msix-build.rs"]
mod msix_build;

fn main() {
    msix_build::resource("explorust", "res/app.manifest", Some("shell"), false);
}
