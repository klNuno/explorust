//! explorust image viewer: one small exe that opens an image at once (WIC decode scaled to the
//! window, then the full decode), browses its folder in Explorer's order and draws with
//! `crates/render` over Mica.

pub mod compose;
pub mod view;
#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
pub use win::run;
