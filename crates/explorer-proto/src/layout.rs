//! Geometry and colors measured on his File Explorer (1240x800 window, 100% scale, dark,
//! inactive, Windhawk MicaBar theme with the acrylic backdrop). Distances from the right
//! or bottom edge are given as negative offsets through the helpers below.

use render::{Color, rgba};

// Bands, top to bottom.
pub const TABSTRIP_H: f32 = 40.0;
pub const NAVBAR_BOTTOM: f32 = 80.0;
pub const CMDBAR_BOTTOM: f32 = 135.0; // 1 px border line at this y
pub const STATUS_H: f32 = 25.0;
pub const HSCROLL_H: f32 = 17.0;
pub const VSCROLL_W: f32 = 17.0;

// Translucent layer over the backdrop (LayerOnMicaBaseAltFillColorDefault, dark).
pub const LAYER: Color = rgba(58, 58, 58, 0.45);
pub const CMDBAR_LINE: Color = rgba(0, 0, 0, 0.19);
pub const TAB_STROKE: Color = rgba(0, 0, 0, 0.10);
pub const CONTROL_FILL: Color = rgba(255, 255, 255, 0.0588);
pub const CONTENT_BG: Color = rgba(25, 25, 25, 1.0);
pub const STATUS_BG: Color = rgba(28, 28, 28, 1.0);
pub const SCROLL_BG: Color = rgba(23, 23, 23, 1.0);
pub const SCROLL_THUMB: Color = rgba(149, 149, 149, 1.0);
pub const TEXT: Color = rgba(255, 255, 255, 1.0);
pub const TEXT_DIM: Color = rgba(207, 207, 207, 1.0);
pub const TEXT_DISABLED: Color = rgba(255, 255, 255, 0.36);
pub const CAPTION_GLYPH: Color = rgba(152, 152, 152, 1.0);
pub const HEADER_SEP: Color = rgba(99, 99, 99, 1.0);
pub const GROUP_CHEVRON: Color = rgba(139, 139, 139, 1.0);
pub const SEL_FILL: Color = rgba(98, 98, 98, 1.0);
pub const SEL_BORDER: Color = rgba(96, 205, 255, 1.0);
pub const HOVER_FILL: Color = rgba(45, 45, 45, 1.0);
pub const NAV_SEL_FILL: Color = rgba(51, 51, 51, 1.0);
pub const NAV_SEL_BORDER: Color = rgba(42, 42, 42, 1.0);
pub const NAV_SEPARATOR: Color = rgba(56, 56, 56, 1.0);
pub const SPLITTER: [(f32, Color); 4] =
    [(0.0, rgba(19, 19, 19, 1.0)), (1.0, rgba(32, 32, 32, 1.0)), (2.0, rgba(43, 43, 43, 1.0)), (3.0, rgba(32, 32, 32, 1.0))];
pub const CMD_SEP: Color = rgba(255, 255, 255, 0.08);
pub const ACCENT_ICON: Color = rgba(96, 205, 255, 1.0);

// Tab.
pub const TAB: (f32, f32, f32, f32) = (9.0, 8.0, 237.0, 40.0);
pub const TAB_RADIUS: f32 = 8.0;
pub const TAB_ICON: (f32, f32) = (17.0, 16.0);
pub const TAB_TEXT_X: f32 = 43.0;
pub const TAB_CLOSE_CX: f32 = 228.5;
pub const TAB_PLUS_CX: f32 = 271.0;
pub const TAB_CY: f32 = 24.0;
/// Caption buttons, centers from the right edge: minimize, maximize, close.
pub const CAPTION_CX: [f32; 3] = [115.0, 69.0, 23.0];
pub const CAPTION_CY: f32 = 16.0;

// Navigation bar.
pub const NAV_BTN_CX: [f32; 4] = [28.0, 76.0, 124.0, 172.0];
pub const NAV_CY: f32 = 64.0;
pub const ADDR: (f32, f32) = (202.0, 315.0); // left, right inset from the window's right edge
pub const BOX_TOP: f32 = 48.0;
pub const BOX_H: f32 = 32.0;
pub const BOX_RADIUS: f32 = 4.0;
pub const ADDR_PC_CX: f32 = 223.0;
pub const CRUMB_FIRST_CHEVRON_CX: f32 = 253.0;
pub const CRUMB_GAP: f32 = 21.0;
pub const SEARCH: (f32, f32) = (306.0, 12.0); // insets from the right edge
pub const SEARCH_TEXT_PAD: f32 = 15.0;
pub const SEARCH_GLYPH_FROM_RIGHT: f32 = 32.0;

// Command bar: (kind, center or text x).
pub const CMD_CY: f32 = 112.0;
pub const CMD_SEPS: [f32; 3] = [122.0, 414.0, 616.0];
pub const CMD_ICONS: [(char, f32, bool); 6] = [
    ('\u{E8C6}', 148.0, true),  // cut
    ('\u{E8C8}', 196.0, true),  // copy
    ('\u{E77F}', 243.0, false), // paste (disabled)
    ('\u{E8AC}', 291.0, true),  // rename
    ('\u{E72D}', 339.0, false), // share (disabled)
    ('\u{E74D}', 388.0, true),  // delete (white, see command_bar)
];
/// Labelled buttons: glyph, glyph cx, label x, chevron cx.
pub const CMD_LABELLED: [(char, f32, f32, f32); 3] =
    [('\u{ECC8}', 26.0, 43.0, 101.0), ('\u{E8CB}', 438.0, 455.0, 488.0), ('\u{E8FD}', 528.0, 544.0, 596.0)];
pub const CMD_MORE_CX: f32 = 641.0;
pub const CMD_DETAILS: (f32, f32) = (77.0, 61.0); // glyph cx, label x, from the right edge

// Navigation pane.
pub const NAV_W: f32 = 238.0;
pub const NAV_FIRST_CY: f32 = 157.0;
pub const NAV_PITCH: f32 = 32.0;
pub const NAV_ICON_X: f32 = 35.0;
pub const NAV_TEXT_X: f32 = 56.0;
pub const NAV_PIN_CX: f32 = 229.0;
pub const NAV_CHEVRON_CX: f32 = 18.0;

// Details view.
pub const LIST_X: f32 = 242.0;
pub const HEADER_BOTTOM: f32 = 160.0;
pub const HEADER_CY: f32 = 148.0;
pub const CONTENT_TOP: f32 = 166.0;
pub const GROUP_H: f32 = 27.0;
pub const ROW_H: f32 = 28.0;
pub const ROW_ICON_X: f32 = 258.0;
pub const ROW_TEXT_X: f32 = 281.0;
pub const HEADER_NAME_X: f32 = 272.0;
pub const GROUP_CHEVRON_CX: f32 = 252.0;
pub const GROUP_TEXT_X: f32 = 263.0;
pub const SEL_X0: f32 = 256.0;
/// Column right edges, from the list's left edge (his saved Downloads layout).
pub const COLUMNS: [f32; 4] = [936.0, 1080.0, 1200.0, 1300.0];
pub const COL_PAD: f32 = 7.0;

// Status bar.
pub const STATUS_TEXT_X: f32 = 15.0;
pub const STATUS_SEP_X: [f32; 2] = [102.0, 238.0];
pub const STATUS_VIEW_CX: [f32; 2] = [48.0, 22.0]; // from the right edge

pub const UI_FONT: &str = "Segoe UI Variable Text";
pub const LIST_FONT: &str = "Segoe UI";
pub const ICON_FONT: &str = "Segoe Fluent Icons";

/// Baseline that centers Segoe text of `size` px on `cy`.
pub fn baseline(cy: f32, size: f32) -> f32 {
    (cy + size * 0.36).round()
}
