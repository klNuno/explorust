//! The chrome above Windows' view, drawn like his File Explorer (dark, MicaBar): tab strip with
//! the caption buttons, navigation bar with the breadcrumb (or its edit box) and the search box,
//! command bar. Mica shows through everything not drawn here.

use crate::edit::LineEdit;
use crate::layout::{self as L, Cmd, Hit};
use crate::text::{self, Strings};
use crate::win::gfx::{Canvas, rect};
use render::{Color, rgba};
use windows::Win32::Graphics::Direct2D::Common::{D2D_SIZE_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    ID2D1Bitmap1,
};
use windows_numerics::Vector2;

const LAYER: Color = rgba(58, 58, 58, 0.45);
const CMDBAR_LINE: Color = rgba(0, 0, 0, 0.19);
const CONTROL_FILL: Color = rgba(255, 255, 255, 0.0588);
const CONTROL_FOCUS: Color = rgba(30, 30, 30, 0.85);
const FOCUS_LINE: Color = rgba(96, 205, 255, 1.0);
const HOVER: Color = rgba(255, 255, 255, 0.0605);
const PRESSED: Color = rgba(255, 255, 255, 0.0419);
const CLOSE_HOVER: Color = rgba(196, 43, 28, 1.0);
const TEXT: Color = rgba(255, 255, 255, 1.0);
const TEXT_DIM: Color = rgba(207, 207, 207, 1.0);
const TEXT_INACTIVE: Color = rgba(255, 255, 255, 0.55);
const TEXT_DISABLED: Color = rgba(255, 255, 255, 0.36);
const CAPTION_GLYPH: Color = rgba(152, 152, 152, 1.0);
const CMD_SEP: Color = rgba(255, 255, 255, 0.08);
const ACCENT_ICON: Color = rgba(96, 205, 255, 1.0);
const SELECTION: Color = rgba(0, 120, 212, 0.75);
const TAB_SEP: Color = rgba(255, 255, 255, 0.12);

pub struct TabView<'a> {
    pub title: &'a str,
    pub icon: Option<&'a ID2D1Bitmap1>,
}

pub struct View<'a> {
    pub w: f32,
    pub tabs: Vec<TabView<'a>>,
    pub active: usize,
    pub can_back: bool,
    pub can_forward: bool,
    pub can_up: bool,
    pub crumbs: Vec<&'a str>,
    pub address: Option<&'a LineEdit>,
    pub search: &'a LineEdit,
    pub search_focus: bool,
    pub hover: Hit,
    pub pressed: Hit,
    pub selection: bool,
    pub details: bool,
    pub window_active: bool,
    pub maximized: bool,
    pub s: &'static Strings,
}

impl View<'_> {
    fn state(&self, h: Hit) -> Option<Color> {
        if self.pressed == h {
            Some(PRESSED)
        } else if self.hover == h {
            Some(HOVER)
        } else {
            None
        }
    }

    fn text_color(&self) -> Color {
        if self.window_active { TEXT } else { TEXT_INACTIVE }
    }
}

/// Draws the chrome; returns the x spans of the breadcrumb items (empty for hidden ones).
pub fn draw(c: &Canvas, v: &View) -> Vec<(f32, f32)> {
    tab_strip(c, v);
    let spans = nav_bar(c, v);
    command_bar(c, v);
    spans
}

fn tab_shape(c: &Canvas, x: f32, w: f32, fill: Color) {
    let (y, h, r) = (L::TAB_Y, L::TABSTRIP_H, L::TAB_RADIUS);
    unsafe {
        let Ok(geo) = c.sh.factory.CreatePathGeometry() else { return };
        let Ok(sink) = geo.Open() else { return };
        let arc = |px: f32, py: f32, sweep| D2D1_ARC_SEGMENT {
            point: Vector2 { X: px, Y: py },
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: sweep,
            arcSize: D2D1_ARC_SIZE_SMALL,
        };
        // Windows 11 tabs flare out into the bar with a small reverse curve at the bottom.
        sink.BeginFigure(Vector2 { X: x - r, Y: y + h }, D2D1_FIGURE_BEGIN_FILLED);
        sink.AddArc(&arc(x, y + h - r, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE));
        sink.AddLine(Vector2 { X: x, Y: y + r });
        sink.AddArc(&arc(x + r, y, D2D1_SWEEP_DIRECTION_CLOCKWISE));
        sink.AddLine(Vector2 { X: x + w - r, Y: y });
        sink.AddArc(&arc(x + w, y + r, D2D1_SWEEP_DIRECTION_CLOCKWISE));
        sink.AddLine(Vector2 { X: x + w, Y: y + h - r });
        sink.AddArc(&arc(x + w + r, y + h, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE));
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        let _ = sink.Close();
        if let Ok(b) = c.ctx.CreateSolidColorBrush(&fill, None) {
            c.ctx.FillGeometry(&geo, &b, None);
        }
    }
}

fn tab_strip(c: &Canvas, v: &View) {
    let tw = L::tab_width(v.w, v.tabs.len());
    for (i, t) in v.tabs.iter().enumerate() {
        let x = L::TAB_X + i as f32 * tw;
        if i == v.active {
            tab_shape(c, x, tw, LAYER);
        } else {
            if let Some(f) = v.state(Hit::Tab(i)) {
                c.round(rect(x + 2.0, L::TAB_Y + 3.0, tw - 4.0, L::TABSTRIP_H - L::TAB_Y - 6.0), 4.0, f);
            }
            if i + 1 != v.active && i + 1 < v.tabs.len() {
                c.fill(rect(x + tw - 0.5, L::TAB_CY - 8.0, 1.0, 16.0), TAB_SEP);
            }
        }
        if let Some(icon) = t.icon {
            c.image(icon, x + L::TAB_ICON_DX, L::TAB_CY - 8.0, 16.0);
        }
        let close = x + tw - L::TAB_CLOSE_FROM_RIGHT;
        let b = L::baseline(L::TAB_CY, 12.0);
        c.text(&c.sh.fonts.ui12, t.title, x + L::TAB_TEXT_DX, close - 14.0, b, v.text_color());
        if let Some(f) = v.state(Hit::TabClose(i)) {
            c.round(rect(close - 12.0, L::TAB_CY - 12.0, 24.0, 24.0), 4.0, f);
        }
        c.glyph(&c.sh.fonts.glyph10, '\u{E8BB}', close, L::TAB_CY, 10.0, v.text_color());
    }
    let plus = L::TAB_X + v.tabs.len() as f32 * tw + L::NEW_TAB_DX;
    if let Some(f) = v.state(Hit::NewTab) {
        c.round(rect(plus - 16.0, L::TAB_CY - 14.0, 32.0, 28.0), 4.0, f);
    }
    c.glyph(&c.sh.fonts.glyph12, '\u{E710}', plus, L::TAB_CY, 12.0, v.text_color());
    let max_glyph = if v.maximized { '\u{E923}' } else { '\u{E922}' };
    for (i, (g, h)) in [('\u{E921}', Hit::Min), (max_glyph, Hit::Max), ('\u{E8BB}', Hit::Close)].into_iter().enumerate() {
        let cx = v.w - L::CAPTION_CX[i];
        let mut glyph = if v.window_active { TEXT } else { CAPTION_GLYPH };
        if v.hover == h || v.pressed == h {
            let fill = if h == Hit::Close { CLOSE_HOVER } else if v.pressed == h { PRESSED } else { HOVER };
            c.fill(rect(cx - L::CAPTION_W / 2.0, 0.0, L::CAPTION_W, L::TABSTRIP_H - 8.0), fill);
            glyph = TEXT;
        }
        c.glyph(&c.sh.fonts.glyph10, g, cx, L::CAPTION_CY, 10.0, glyph);
    }
}

fn nav_bar(c: &Canvas, v: &View) -> Vec<(f32, f32)> {
    let w = v.w;
    c.fill(rect(0.0, L::TABSTRIP_H, w, L::NAVBAR_BOTTOM - L::TABSTRIP_H + 8.0), LAYER);
    let buttons = [
        ('\u{E72B}', Hit::Back, v.can_back),
        ('\u{E72A}', Hit::Forward, v.can_forward),
        ('\u{E74A}', Hit::Up, v.can_up),
        ('\u{E72C}', Hit::Refresh, true),
    ];
    for (i, (g, h, on)) in buttons.into_iter().enumerate() {
        let cx = L::NAV_BTN_CX[i];
        if on {
            if let Some(f) = v.state(h) {
                c.round(rect(cx - 18.0, L::BOX_TOP, 36.0, L::BOX_H), 4.0, f);
            }
        }
        c.glyph(&c.sh.fonts.glyph14, g, cx, L::NAV_CY, 14.0, if on { TEXT } else { TEXT_DISABLED });
    }
    let (ax, ar) = L::ADDR;
    let b = L::baseline(L::NAV_CY, 14.0);
    let f14 = &c.sh.fonts.ui14;
    let mut spans = vec![(0.0, 0.0); v.crumbs.len()];
    if let Some(edit) = v.address {
        editor(c, edit, ax, w - ar, b, "");
    } else {
        let fill = if v.hover == Hit::Address { rgba(255, 255, 255, 0.0837) } else { CONTROL_FILL };
        c.round(rect(ax, L::BOX_TOP, w - ar - ax, L::BOX_H), L::BOX_RADIUS, fill);
        c.glyph(&c.sh.fonts.glyph16, '\u{E8B7}', L::ADDR_ICON_CX, L::NAV_CY, 16.0, TEXT);
        let widths: Vec<f32> = v.crumbs.iter().map(|s| c.measure(f14, s)).collect();
        let room = w - ar - L::CRUMB_FIRST_CHEVRON_CX - 10.0;
        let first = L::first_fitting(&widths, L::CRUMB_GAP, room);
        let mut chevron = L::CRUMB_FIRST_CHEVRON_CX;
        if first > 0 {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76B}', chevron - 2.0, L::NAV_CY, 10.0, TEXT);
            chevron += 14.0;
        }
        for i in first..v.crumbs.len() {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, TEXT);
            let x = chevron + L::CRUMB_GAP + 1.0;
            let x1 = (x + widths[i]).min(w - ar - 4.0);
            spans[i] = (x - 6.0, x1 + 6.0);
            if let Some(f) = v.state(Hit::Crumb(i)) {
                c.round(rect(x - 6.0, L::BOX_TOP + 3.0, x1 - x + 12.0, L::BOX_H - 6.0), 4.0, f);
            }
            c.text(f14, v.crumbs[i], x, x1, b, TEXT);
            chevron = x + widths[i] + L::CRUMB_GAP;
        }
        if chevron < w - ar - 8.0 {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, TEXT);
        }
    }
    let (sl, sr) = L::SEARCH;
    let title = v.crumbs.last().copied().unwrap_or("");
    if v.search_focus {
        editor(c, v.search, w - sl, w - sr - 30.0, b, &text::search_hint(v.s, title));
    } else {
        let fill = if v.hover == Hit::Search { rgba(255, 255, 255, 0.0837) } else { CONTROL_FILL };
        c.round(rect(w - sl, L::BOX_TOP, sl - sr, L::BOX_H), L::BOX_RADIUS, fill);
        let shown = if v.search.is_empty() { text::search_hint(v.s, title) } else { v.search.text() };
        let color = if v.search.is_empty() { TEXT_DIM } else { TEXT };
        c.text(f14, &shown, w - sl + L::SEARCH_TEXT_PAD, w - L::SEARCH_GLYPH_FROM_RIGHT - 12.0, b, color);
    }
    c.glyph(&c.sh.fonts.glyph12, '\u{E721}', w - L::SEARCH_GLYPH_FROM_RIGHT, L::NAV_CY, 12.0, TEXT);
    spans
}

/// A focused text box in [x0, x1]: darker fill, accent underline, the text (or its hint), the
/// selection or the caret.
fn editor(c: &Canvas, e: &LineEdit, x0: f32, x1: f32, baseline: f32, hint: &str) {
    let f = &c.sh.fonts.ui14;
    c.round(rect(x0, L::BOX_TOP, x1 - x0 + if hint.is_empty() { 0.0 } else { 30.0 }, L::BOX_H), L::BOX_RADIUS, CONTROL_FOCUS);
    c.fill(rect(x0 + 1.0, L::BOX_TOP + L::BOX_H - 2.0, x1 - x0 - 2.0, 2.0), FOCUS_LINE);
    let tx = x0 + 11.0;
    let s = e.text();
    if s.is_empty() && !hint.is_empty() {
        c.text(f, hint, tx, x1 - 8.0, baseline, TEXT_DIM);
    }
    if e.all && !s.is_empty() {
        let wsel = c.measure(f, &s).min(x1 - tx - 8.0);
        c.fill(rect(tx, L::BOX_TOP + 7.0, wsel, L::BOX_H - 14.0), SELECTION);
    }
    c.text(f, &s, tx, x1 - 8.0, baseline, TEXT);
    if !e.all {
        let cx = (tx + c.caret_x(f, &s, e.caret_utf16())).min(x1 - 8.0);
        c.fill(rect(cx.round(), L::BOX_TOP + 8.0, 1.0, L::BOX_H - 16.0), TEXT);
    }
}

fn command_bar(c: &Canvas, v: &View) {
    let w = v.w;
    c.fill(rect(0.0, L::NAVBAR_BOTTOM + 8.0, w, L::CMDBAR_BOTTOM - L::NAVBAR_BOTTOM - 8.0), LAYER);
    c.fill(rect(0.0, L::CMDBAR_BOTTOM, w, 1.0), CMDBAR_LINE);
    let cy = L::CMD_CY;
    let b = L::baseline(cy, 12.0);
    let labels = [v.s.new, v.s.sort, v.s.view];
    for ((cmd, g, gx, tx, chev, l, r), label) in L::CMD_LABELLED.into_iter().zip(labels) {
        if let Some(f) = v.state(Hit::Cmd(cmd)) {
            c.round(rect(l, cy - L::CMD_H / 2.0, r - l, L::CMD_H), 4.0, f);
        }
        let color = if cmd == Cmd::New { ACCENT_ICON } else { TEXT };
        c.glyph(&c.sh.fonts.glyph16, g, gx, cy, 16.0, color);
        c.text(&c.sh.fonts.ui12, label, tx, tx + 80.0, b, TEXT);
        c.glyph(&c.sh.fonts.glyph10, '\u{E70D}', chev, cy, 8.0, TEXT_DIM);
    }
    for x in L::CMD_SEPS {
        c.fill(rect(x, cy - 16.0, 1.0, 32.0), CMD_SEP);
    }
    for (cmd, g, cx) in L::CMD_ICONS {
        let on = match cmd {
            Cmd::Paste => true,
            _ => v.selection,
        };
        if on {
            if let Some(f) = v.state(Hit::Cmd(cmd)) {
                c.round(rect(cx - L::CMD_BTN_W / 2.0, cy - L::CMD_H / 2.0, L::CMD_BTN_W, L::CMD_H), 4.0, f);
            }
        }
        let color = if !on { TEXT_DISABLED } else if cmd == Cmd::Delete { TEXT } else { ACCENT_ICON };
        c.glyph(&c.sh.fonts.glyph16, g, cx, cy, 16.0, color);
    }
    if let Some(f) = v.state(Hit::Cmd(Cmd::More)) {
        c.round(rect(L::CMD_MORE_CX - 20.0, cy - 16.0, 40.0, 32.0), 4.0, f);
    }
    c.glyph(&c.sh.fonts.glyph16, '\u{E712}', L::CMD_MORE_CX, cy, 16.0, TEXT);
    let (gx, tx, l) = L::CMD_DETAILS;
    if v.details || v.state(Hit::Cmd(Cmd::Details)).is_some() {
        let f = v.state(Hit::Cmd(Cmd::Details)).unwrap_or(PRESSED);
        c.round(rect(w - l, cy - 16.0, l - 8.0, 32.0), 4.0, f);
    }
    c.glyph(&c.sh.fonts.glyph16, '\u{E8A0}', w - gx, cy, 16.0, ACCENT_ICON);
    c.text(&c.sh.fonts.ui12, v.s.details, w - tx, w - 8.0, b, TEXT);
}
