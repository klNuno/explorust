//! Geometry and colors of the owner's Windows 11 taskbar at 100% scale, measured on his
//! screen (2560x1440, centered, auto-hide, Windhawk TranslucentTaskbar). Shared by both
//! candidates so they differ only by renderer.

pub const BAR_H: f32 = 48.0;
pub const SLOT: f32 = 44.0;
pub const ICON: f32 = 24.0;
pub const ICON_TOP: f32 = 12.0;
pub const BUTTON_BG: (f32, f32, f32, f32) = (-20.0, 4.0, 40.0, 40.0); // x offset from center, y, w, h
pub const BUTTON_RADIUS: f32 = 4.0;
pub const INDICATOR_Y: f32 = 40.0;
pub const INDICATOR_H: f32 = 3.0;
pub const RUNNING_W: f32 = 6.0;
pub const ACTIVE_W: f32 = 16.0;
pub const RUNNING_RGB: (u8, u8, u8) = (155, 155, 156);
pub const ACTIVE_RGB: (u8, u8, u8) = (181, 173, 235);
/// White overlay alpha on a button: active window, hover, both.
pub const ACTIVE_BG_A: f32 = 0.07;
pub const HOVER_BG_A: f32 = 0.09;
/// Pointer-over fade, XAML ControlFasterAnimationDuration (83 ms) in, 167 ms out.
pub const HOVER_IN_MS: f32 = 83.0;
pub const HOVER_OUT_MS: f32 = 167.0;

/// Start logo: four 11 px squares, 1 px gap, diagonal gradient.
pub const START_SQ: f32 = 11.0;
pub const START_TOP_LEFT: (u8, u8, u8) = (128, 224, 255);
pub const START_BOTTOM_RIGHT: (u8, u8, u8) = (14, 158, 255);

/// Light plate behind the app buttons seen on his bar: half-intensity edges at these
/// offsets from the group's left and right ends, 70 px feather, 211 at top to 170 at bottom.
pub const PLATE_LEFT: f32 = 74.0;
pub const PLATE_RIGHT: f32 = 50.0;
pub const PLATE_FEATHER: f32 = 70.0;
pub const PLATE_TOP: u8 = 211;
pub const PLATE_BOTTOM: u8 = 170;

/// Backdrop tint over the blur (ABGR for the DWM accent).
pub const TINT_ABGR: u32 = 0x40_1c_1a_1a;

/// Clock: right edge of the text from the bar's right end, baselines from the top.
pub const CLOCK_RIGHT: f32 = 19.0;
pub const LINE1_BASELINE: f32 = 19.5;
pub const LINE2_BASELINE: f32 = 35.5;
pub const TEXT_PX: f32 = 12.0;
pub const GLYPH_PX: f32 = 16.0;

/// Tray items, centers measured from the bar's right end.
pub enum TrayItem {
    Glyph(char),
    App(&'static str),
    TwoLines(&'static str, &'static str),
}

pub const TRAY: &[(f32, TrayItem)] = &[
    (396.0, TrayItem::Glyph('\u{E70E}')), // overflow chevron
    (370.0, TrayItem::App("boite")),
    (338.0, TrayItem::App("steam")),
    (306.0, TrayItem::Glyph('\u{F0E2}')), // grid dots
    (275.0, TrayItem::Glyph('\u{EA3A}')), // ring
    (243.0, TrayItem::Glyph('\u{E895}')), // sync
    (210.0, TrayItem::Glyph('\u{E720}')), // microphone in use
    (171.0, TrayItem::TwoLines("FRA", "INTL")),
    (135.0, TrayItem::Glyph('\u{E839}')), // ethernet
    (106.0, TrayItem::Glyph('\u{E995}')), // volume
];

/// Left edge of the centered button group (Start included).
pub fn group_x0(bar_w: f32, buttons: usize) -> f32 {
    (bar_w / 2.0 - buttons as f32 * SLOT / 2.0).round()
}

/// Center x of button `i` (0 = Start).
pub fn button_cx(bar_w: f32, buttons: usize, i: usize) -> f32 {
    group_x0(bar_w, buttons) + SLOT / 2.0 + SLOT * i as f32
}

/// Hover fade step: moves `v` toward `target` for `dt_ms`, returns whether still moving.
pub fn fade(v: &mut f32, target: f32, dt_ms: f32) -> bool {
    let dur = if target > *v { HOVER_IN_MS } else { HOVER_OUT_MS };
    let step = dt_ms / dur;
    if target > *v { *v = (*v + step).min(target) } else { *v = (*v - step).max(target) }
    (*v - target).abs() > f32::EPSILON
}

/// The light plate as a straight-alpha BGRA image of the bar's size.
pub fn plate_image(bar_w: u32, buttons: usize) -> crate::Image {
    let h = BAR_H as u32;
    let x0 = group_x0(bar_w as f32, buttons);
    let (l, r) = (x0 + PLATE_LEFT, x0 + buttons as f32 * SLOT - PLATE_RIGHT);
    let mut bgra = vec![0u8; (bar_w * h * 4) as usize];
    for y in 0..h {
        let t = y as f32 / (h - 1) as f32;
        let g = (PLATE_TOP as f32 + (PLATE_BOTTOM as f32 - PLATE_TOP as f32) * t).round() as u8;
        for x in 0..bar_w {
            let xf = x as f32 + 0.5;
            let a = (((xf - l) / PLATE_FEATHER + 0.5).clamp(0.0, 1.0)).min(((r - xf) / PLATE_FEATHER + 0.5).clamp(0.0, 1.0));
            let a = a * a * (3.0 - 2.0 * a);
            let i = ((y * bar_w + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[g, g, g, (a * 255.0).round() as u8]);
        }
    }
    crate::Image { width: bar_w, height: h, bgra }
}

/// The Start logo, 23x23 straight-alpha BGRA.
pub fn start_image() -> crate::Image {
    let n = (START_SQ * 2.0 + 1.0) as u32;
    let mut bgra = vec![0u8; (n * n * 4) as usize];
    let (a, b) = (START_TOP_LEFT, START_BOTTOM_RIGHT);
    for y in 0..n {
        for x in 0..n {
            if x == START_SQ as u32 || y == START_SQ as u32 {
                continue;
            }
            let t = (x + y) as f32 / (2 * (n - 1)) as f32;
            let m = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t).round() as u8;
            let i = ((y * n + x) * 4) as usize;
            bgra[i..i + 4].copy_from_slice(&[m(a.2, b.2), m(a.1, b.1), m(a.0, b.0), 255]);
        }
    }
    crate::Image { width: n, height: n, bgra }
}
