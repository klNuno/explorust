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
pub use model::CommandItem;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
pub use win::{ClipboardRoute, TrackingTip, hook, hook_with_clipboard, prewarm, show, track_commands};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardVerb {
    Cut,
    Copy,
    Paste,
}

impl ClipboardVerb {
    pub fn from_canonical(verb: &str) -> Option<Self> {
        [("cut", Self::Cut), ("copy", Self::Copy), ("paste", Self::Paste)]
            .into_iter()
            .find_map(|(name, command)| name.eq_ignore_ascii_case(verb).then_some(command))
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardVerb;

    #[test]
    fn only_canonical_file_clipboard_verbs_are_routed() {
        assert_eq!(ClipboardVerb::from_canonical("cut"), Some(ClipboardVerb::Cut));
        assert_eq!(ClipboardVerb::from_canonical("COPY"), Some(ClipboardVerb::Copy));
        assert_eq!(ClipboardVerb::from_canonical("Paste"), Some(ClipboardVerb::Paste));
        for native in ["", "pastelink", "copyaspath", "Copy as path", "sendto"] {
            assert_eq!(ClipboardVerb::from_canonical(native), None);
        }
    }
}
