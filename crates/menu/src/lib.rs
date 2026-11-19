//! The Windows 11 context menu for the parts that host a shell view (Files, the desktop).
//!
//! The data is Windows' own: the view's `IContextMenu` (selection or background) fills an HMENU
//! with `QueryContextMenu`, which is read item by item (text, verb, state, bitmap, submenus filled
//! through `IContextMenu3::HandleMenuMsg2`). The menu is drawn as Windows 11 draws it: a top row
//! of icon buttons (cut, copy, rename, share, delete), items with Segoe Fluent Icons glyphs and
//! shortcuts, submenus, `Show more options` (the classic menu of the same HMENU), rounded corners,
//! the DWM shadow and acrylic, Direct2D through `crates/render`. A command runs through
//! `InvokeCommand` with its id, the point and the modifier keys.
//!
//! A part calls [`hook`] with each shell view it creates: right-click, the menu key and
//! Shift+F10 on that view then show this menu instead of the classic one.

pub mod model;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
pub use win::{hook, prewarm, show};
