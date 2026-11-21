//! The chrome above Windows' view, drawn like his File Explorer (dark, MicaBar): tab strip with
//! the caption buttons, navigation bar with the breadcrumb (or its edit box) and the search box,
//! command bar. Mica shows through everything not drawn here.

use crate::edit::LineEdit;
use crate::layout::{self as L, Cmd, Hit};
use crate::text::{self, Strings};
use crate::win::gfx::{Canvas, rect};
use render::{Color, rgba};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

const TITLE_LAYER: Color = rgba(25, 27, 30, 0.70);
const LAYER: Color = rgba(39, 42, 46, 0.62);
const TAB_ACTIVE: Color = rgba(255, 255, 255, 0.11);
const CMDBAR_LINE: Color = rgba(255, 255, 255, 0.09);
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
    pub paste: bool,
    pub create: bool,
    pub ready: bool,
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

pub fn status(c: &Canvas, v: &View, items: i32, selected: i32, details: bool) {
    let h = L::STATUS_H;
    c.fill(rect(0.0, 0.0, v.w, h), LAYER);
    c.fill(rect(0.0, 0.0, v.w, 1.0), CMDBAR_LINE);
    if v.ready {
        let mut summary = format!("{items} {}", v.s.items[usize::from(items != 1)]);
        if selected > 0 {
            use std::fmt::Write;
            let noun = v.s.selected[usize::from(selected != 1)];
            let _ = write!(summary, "   |   {selected} {noun}");
        }
        c.text(&c.sh.fonts.ui12, &summary, 12.0, v.w - 80.0, L::baseline(h / 2.0, 12.0), TEXT_DIM);
    }
    for (hit, glyph, from_right, active) in [
        (Hit::StatusDetails, '\u{E8FD}', L::STATUS_DETAILS_FROM_RIGHT, details),
        (Hit::StatusIcons, '\u{E8A9}', L::STATUS_ICONS_FROM_RIGHT, !details),
    ] {
        let x = v.w - from_right;
        if v.ready {
            if let Some(fill) = v.state(hit).or_else(|| active.then_some(TAB_ACTIVE)) {
                c.round(rect(x - 14.0, 3.0, 28.0, h - 6.0), 4.0, fill);
            }
        }
        c.glyph(&c.sh.fonts.glyph14, glyph, x, h / 2.0, 14.0, if v.ready { TEXT_DIM } else { TEXT_DISABLED });
    }
}

fn tab_strip(c: &Canvas, v: &View) {
    c.fill(rect(0.0, 0.0, v.w, L::TABSTRIP_H), TITLE_LAYER);
    let strip = L::tab_strip(v.w, v.tabs.len(), v.active);
    let width = strip.width - L::TAB_GAP;
    for (slot, i) in strip.visible.enumerate() {
        let t = &v.tabs[i];
        let x = L::TAB_X + slot as f32 * strip.width;
        let background = v.state(Hit::Tab(i)).or_else(|| (i == v.active).then_some(TAB_ACTIVE));
        if let Some(fill) = background {
            c.round(rect(x, L::TAB_Y, width, L::TABSTRIP_H - L::TAB_Y - 4.0), L::TAB_RADIUS, fill);
        }
        if let Some(icon) = t.icon {
            c.image(icon, x + L::TAB_ICON_DX, L::TAB_CY - 8.0, 16.0);
        } else {
            c.glyph(&c.sh.fonts.glyph14, '\u{E8B7}', x + L::TAB_ICON_DX + 8.0, L::TAB_CY, 14.0, TEXT_DIM);
        }
        let close = x + width - L::TAB_CLOSE_FROM_RIGHT;
        let b = L::baseline(L::TAB_CY, 12.0);
        let title = if t.title.is_empty() { v.s.home } else { t.title };
        c.text(&c.sh.fonts.ui12, title, x + L::TAB_TEXT_DX, close - 13.0, b, v.text_color());
        tab_button(c, v, Hit::TabClose(i), '\u{E8BB}', close, 24.0);
    }
    tab_button(c, v, Hit::NewTab, '\u{E710}', strip.plus, 30.0);
    if let Some(x) = strip.left {
        tab_button(c, v, Hit::TabsBack, '\u{E76B}', x, 26.0);
    }
    if let Some(x) = strip.right {
        tab_button(c, v, Hit::TabsForward, '\u{E76C}', x, 26.0);
    }
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

fn tab_button(c: &Canvas, v: &View, hit: Hit, glyph: char, x: f32, width: f32) {
    if let Some(fill) = v.state(hit) {
        c.round(rect(x - width / 2.0, L::TAB_CY - 12.0, width, 24.0), 5.0, fill);
    }
    c.glyph(&c.sh.fonts.glyph10, glyph, x, L::TAB_CY, 10.0, v.text_color());
}

fn nav_bar(c: &Canvas, v: &View) -> Vec<(f32, f32)> {
    let w = v.w;
    c.fill(rect(0.0, L::TABSTRIP_H, w, L::NAVBAR_BOTTOM - L::TABSTRIP_H), LAYER);
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
    let ax = L::ADDR_X;
    let sl = L::search_left(w);
    let ar = sl - 8.0;
    let b = L::baseline(L::NAV_CY, 14.0);
    let f14 = &c.sh.fonts.ui14;
    let mut spans = vec![(0.0, 0.0); v.crumbs.len()];
    if let Some(edit) = v.address {
        editor(c, edit, ax, ar, b, "", 0.0);
    } else {
        let fill = if v.hover == Hit::Address { rgba(255, 255, 255, 0.0837) } else { CONTROL_FILL };
        c.round(rect(ax, L::BOX_TOP, ar - ax, L::BOX_H), L::BOX_RADIUS, fill);
        c.glyph(&c.sh.fonts.glyph16, '\u{E8B7}', L::ADDR_ICON_CX, L::NAV_CY, 16.0, TEXT);
        let widths: Vec<f32> = v.crumbs.iter().map(|s| c.measure(f14, s)).collect();
        let room = ar - L::CRUMB_FIRST_CHEVRON_CX - 10.0;
        let first = L::first_fitting(&widths, L::CRUMB_GAP, room);
        let mut chevron = L::CRUMB_FIRST_CHEVRON_CX;
        if first > 0 {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76B}', chevron - 2.0, L::NAV_CY, 10.0, TEXT);
            chevron += 14.0;
        }
        for i in first..v.crumbs.len() {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, TEXT);
            let x = chevron + L::CRUMB_GAP + 1.0;
            let x1 = (x + widths[i]).min(ar - 4.0);
            spans[i] = (x - 6.0, x1 + 6.0);
            if let Some(f) = v.state(Hit::Crumb(i)) {
                c.round(rect(x - 6.0, L::BOX_TOP + 3.0, x1 - x + 12.0, L::BOX_H - 6.0), 4.0, f);
            }
            c.text(f14, v.crumbs[i], x, x1, b, TEXT);
            chevron = x + widths[i] + L::CRUMB_GAP;
        }
        if chevron < ar - 8.0 {
            c.glyph(&c.sh.fonts.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, TEXT);
        }
    }
    let sr = w - L::RIGHT_PAD;
    let title = v.crumbs.last().copied().unwrap_or("");
    if v.search_focus {
        editor(c, v.search, sl, sr, b, &text::search_hint(v.s, title), 30.0);
    } else {
        let fill = if v.hover == Hit::Search { rgba(255, 255, 255, 0.0837) } else { CONTROL_FILL };
        c.round(rect(sl, L::BOX_TOP, sr - sl, L::BOX_H), L::BOX_RADIUS, fill);
        let shown = if v.search.is_empty() { text::search_hint(v.s, title) } else { v.search.text() };
        let color = if v.search.is_empty() { TEXT_DIM } else { TEXT };
        c.text(f14, &shown, sl + L::SEARCH_TEXT_PAD, w - L::SEARCH_GLYPH_FROM_RIGHT - 12.0, b, color);
    }
    c.glyph(&c.sh.fonts.glyph12, '\u{E721}', w - L::SEARCH_GLYPH_FROM_RIGHT, L::NAV_CY, 12.0, TEXT);
    spans
}

/// A focused text box in [x0, x1]: darker fill, accent underline, the text (or its hint), the
/// selection or the caret.
fn editor(c: &Canvas, e: &LineEdit, x0: f32, x1: f32, baseline: f32, hint: &str, trailing: f32) {
    let f = &c.sh.fonts.ui14;
    c.round(rect(x0, L::BOX_TOP, x1 - x0, L::BOX_H), L::BOX_RADIUS, CONTROL_FOCUS);
    c.fill(rect(x0 + 1.0, L::BOX_TOP + L::BOX_H - 2.0, x1 - x0 - 2.0, 2.0), FOCUS_LINE);
    let x1 = x1 - trailing;
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
    c.fill(rect(0.0, L::NAVBAR_BOTTOM, w, L::CMDBAR_BOTTOM - L::NAVBAR_BOTTOM), LAYER);
    c.fill(rect(0.0, L::CMDBAR_BOTTOM, w, 1.0), CMDBAR_LINE);
    let cy = L::CMD_CY;
    let b = L::baseline(cy, 12.0);
    let labels = [v.s.new, v.s.sort, v.s.view];
    for ((cmd, g, gx, tx, chev, l, r), label) in L::CMD_LABELLED.into_iter().zip(labels) {
        let (gx, l, r) = L::label_bounds(w, cmd, gx, l, r);
        let on = v.ready && (cmd != Cmd::New || v.create);
        if on {
            if let Some(f) = v.state(Hit::Cmd(cmd)) {
                c.round(rect(l, cy - L::CMD_H / 2.0, r - l, L::CMD_H), 5.0, f);
            }
        }
        let color = if !on { TEXT_DISABLED } else if cmd == Cmd::New { ACCENT_ICON } else { TEXT };
        c.glyph(&c.sh.fonts.glyph16, g, gx, cy, 16.0, color);
        if cmd == Cmd::New || !L::compact_commands(w) {
            let text_right = if cmd == Cmd::New { r - 4.0 } else { chev - 7.0 };
            c.text(&c.sh.fonts.ui12, label, tx, text_right, b, if on { TEXT } else { TEXT_DISABLED });
            if cmd != Cmd::New {
                c.glyph(&c.sh.fonts.glyph10, '\u{E70D}', chev, cy, 8.0, if on { TEXT_DIM } else { TEXT_DISABLED });
            }
        }
    }
    for x in L::CMD_SEPS {
        c.fill(rect(x, cy - 16.0, 1.0, 32.0), CMD_SEP);
    }
    for (cmd, g, cx) in L::CMD_ICONS {
        let on = match cmd {
            Cmd::Paste => v.paste,
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
    let more = L::command_more_x(w);
    if v.ready {
        if let Some(f) = v.state(Hit::Cmd(Cmd::More)) {
            c.round(rect(more - 17.0, cy - 16.0, 34.0, 32.0), 5.0, f);
        }
    }
    c.glyph(&c.sh.fonts.glyph16, '\u{E712}', more, cy, 16.0, if v.ready { TEXT } else { TEXT_DISABLED });
    let (mut gx, tx, mut l) = L::CMD_DETAILS;
    if L::compact_commands(w) {
        gx = 24.0;
        l = 40.0;
    }
    if v.details || v.state(Hit::Cmd(Cmd::Details)).is_some() {
        let f = v.state(Hit::Cmd(Cmd::Details)).unwrap_or(PRESSED);
        c.round(rect(w - l, cy - 16.0, l - 8.0, 32.0), 4.0, f);
    }
    c.glyph(&c.sh.fonts.glyph16, '\u{E8A0}', w - gx, cy, 16.0, ACCENT_ICON);
    if !L::compact_commands(w) {
        c.text(&c.sh.fonts.ui12, v.s.details, w - tx, w - 8.0, b, TEXT);
    }
}
