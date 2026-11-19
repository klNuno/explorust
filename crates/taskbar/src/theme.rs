//! Colors of Windows 11's bar in light and dark mode. Light is measured on explorer's bar in the
//! guest (`captures/step1-fallback-kill.png`), dark on the owner's bar (`.agents/taskbar-result.md`;
//! its attention button in `captures/ref-strip.png`: fill 127,67,74 opaque, indicator 255,153,164
//! 20 px wide). Light attention colors are Windows' critical fill colors, not measured.

/// sRGB channels and an alpha in 0..1.
pub type Rgba = (u8, u8, u8, f32);

pub struct Theme {
    pub light: bool,
    /// DWM accent tint under the drawing (ABGR).
    pub tint_abgr: u32,
    /// 1 px line along the top edge.
    pub top_line: Rgba,
    pub text: Rgba,
    /// Fill of the active app's button, and its 1 px stroke.
    pub active_bg: Rgba,
    pub active_stroke: Rgba,
    /// Extra darkening of the stroke's bottom edge (Windows' elevation border).
    pub active_stroke_bottom: Rgba,
    /// Added on hover (scaled by the hover fade).
    pub hover_bg: Rgba,
    pub press_bg: Rgba,
    /// Attention state (FlashWindowEx): the button's fill and its indicator, 20 px wide.
    pub attention_bg: Rgba,
    pub attention_ind: Rgba,
    pub running: Rgba,
    pub active_ind: Rgba,
    pub focus: Rgba,
}

impl Theme {
    pub fn new(light: bool, accent: (u8, u8, u8)) -> Theme {
        if light {
            Theme {
                light,
                tint_abgr: 0xFF_EE_EE_EE,
                top_line: (190, 190, 190, 1.0),
                text: (0, 0, 0, 0.896),
                active_bg: (255, 255, 255, 0.53),
                active_stroke: (0, 0, 0, 0.03),
                active_stroke_bottom: (0, 0, 0, 0.06),
                hover_bg: (255, 255, 255, 0.53),
                press_bg: (255, 255, 255, 0.30),
                attention_bg: (253, 231, 233, 1.0),
                attention_ind: (196, 43, 28, 1.0),
                running: (132, 132, 132, 1.0),
                active_ind: (accent.0, accent.1, accent.2, 1.0),
                focus: (0, 0, 0, 0.9),
            }
        } else {
            Theme {
                light,
                tint_abgr: 0x40_1c_1a_1a,
                top_line: (255, 255, 255, 0.08),
                text: (255, 255, 255, 1.0),
                active_bg: (255, 255, 255, 0.07),
                active_stroke: (255, 255, 255, 0.0),
                active_stroke_bottom: (255, 255, 255, 0.0),
                hover_bg: (255, 255, 255, 0.09),
                press_bg: (255, 255, 255, 0.05),
                attention_bg: (127, 67, 74, 1.0),
                attention_ind: (255, 153, 164, 1.0),
                running: (155, 155, 156, 1.0),
                active_ind: (181, 173, 235, 1.0),
                focus: (255, 255, 255, 0.9),
            }
        }
    }
}
