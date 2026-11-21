//! Start's state and its drawing: Direct2D on the composition swap chain of `render`, DirectWrite
//! text in Segoe UI Variable, Segoe Fluent Icons glyphs, shell icons as D2D bitmaps.

use crate::{apps::App, edit::Edit};
use crate::layout::{self as L, Hit, Rect};
use crate::model::{self, Line};
use crate::recent::Recent;
use crate::strings::Strings;
use render::text::{self, Align, Text};
use render::{Color, Surface, rgba};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1,
    ID2D1Brush, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_WRAP, IDWriteTextFormat,
};
use windows_core::{Interface, Result};
use windows_numerics::{Matrix3x2, Vector2};

mod results;
mod search_field;
pub use results::{Act, FileEntry, Found};

const WHITE: Color = rgba(255, 255, 255, 1.0);
const SECONDARY: Color = rgba(255, 255, 255, 0.786);
const TERTIARY: Color = rgba(255, 255, 255, 0.545);
const HOVER: Color = rgba(255, 255, 255, 0.0605);
const STROKE: Color = rgba(255, 255, 255, 0.07);

const GLYPH_SEARCH: &str = "\u{E721}";
const GLYPH_CHEVRON_R: &str = "\u{E76C}";
const GLYPH_CHEVRON_L: &str = "\u{E76B}";
const GLYPH_POWER: &str = "\u{E7E8}";
const GLYPH_USER: &str = "\u{E77B}";
const GLYPH_DOC: &str = "\u{E8A5}";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Main,
    AllApps,
    Search,
}

pub struct AppEntry {
    pub name: String,
    pub aumid: String,
    pub icon: Option<ID2D1Bitmap1>,
    pub installed: Option<i64>,
}

pub enum RecKind {
    File(String),
    App(String),
}

pub struct RecEntry {
    pub title: String,
    pub subtitle: String,
    pub icon: Option<ID2D1Bitmap1>,
    pub kind: RecKind,
}

pub struct Start {
    pub hwnd: HWND,
    pub scale: f32,
    surface: Surface,
    text: Text,
    brush: ID2D1SolidColorBrush,
    base: ID2D1Brush,
    label_wrap: IDWriteTextFormat,
    label_left: IDWriteTextFormat,
    header: IDWriteTextFormat,
    body: IDWriteTextFormat,
    search_text: IDWriteTextFormat,
    selection_color: Color,
    search_offset: f32,
    glyph: IDWriteTextFormat,
    glyph_small: IDWriteTextFormat,
    title: IDWriteTextFormat,
    center: IDWriteTextFormat,
    glyph_big: IDWriteTextFormat,
    pub s: &'static Strings,
    pub user: String,
    pub apps: Vec<AppEntry>,
    pub lines: Vec<Line>,
    pub pins: Vec<String>,
    pub recs: Vec<RecEntry>,
    pub view: View,
    pub page: usize,
    pub scroll: f32,
    pub query: String,
    pub edit: Edit,
    pub search_drag: bool,
    pub found: Found,
    /// Count and oldest message time of text keys waiting for a frame.
    pub pending_search_keys: Option<(u32, u32)>,
    /// Search entries of `apps` (same order), their use counts, and the Settings pages.
    pub app_index: Vec<crate::search::Entry>,
    pub app_uses: Vec<u32>,
    pub usage: Vec<(String, u32)>,
    pub settings: Vec<crate::sources::Setting>,
    pub settings_index: Vec<crate::search::Entry>,
    pub hover: Hit,
    pub search_focus: bool,
    /// Keyboard focus (Tab, arrows), drawn with Windows' focus rectangle.
    pub focus: Hit,
    pub shown: bool,
    pub menu_open: bool,
    pub prev_fg: HWND,
    pub dirty: bool,
    pub loaded: bool,
}

fn trimmed(text: &Text, f: IDWriteTextFormat) -> Result<IDWriteTextFormat> {
    // SAFETY: sets character trimming with an ellipsis on a format we own.
    unsafe {
        let sign = text.factory.CreateEllipsisTrimmingSign(&f)?;
        let t = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        f.SetTrimming(&t, &sign)?;
    }
    Ok(f)
}

fn r(x: &Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: x.x, top: x.y, right: x.right(), bottom: x.bottom() }
}

impl Start {
    pub fn new(hwnd: HWND, scale: f32, s: &'static Strings, user: String) -> Result<Self> {
        let surface = Surface::with_buffers(hwnd, (L::W * scale).round() as u32, (L::H * scale).round() as u32, 3)?;
        let text = Text::new()?;
        let brush = surface.brush(WHITE)?;
        let base: ID2D1Brush = brush.cast()?;
        let ui = "Segoe UI Variable Text";
        let label_wrap = trimmed(&text, text.format(ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?)?;
        // SAFETY: wrapping on our own format.
        unsafe { label_wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)? };
        let label_left = trimmed(&text, text.format(ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?)?;
        let header = text.format(ui, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, Align::Leading)?;
        let body = trimmed(&text, text.format(ui, 14.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?)?;
        let search_text = text.format(ui, 14.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading)?;
        let glyph = text.format("Segoe Fluent Icons", 16.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let glyph_small = text.format("Segoe Fluent Icons", 8.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let title = trimmed(&text, text.format("Segoe UI Variable Display", 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, Align::Center)?)?;
        let center = trimmed(&text, text.format(ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?)?;
        let glyph_big = text.format("Segoe Fluent Icons", 40.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        Ok(Self {
            hwnd,
            scale,
            surface,
            text,
            brush,
            base,
            label_wrap,
            label_left,
            header,
            body,
            search_text,
            selection_color: search_field::accent(),
            search_offset: 0.0,
            glyph,
            glyph_small,
            title,
            center,
            glyph_big,
            s,
            user,
            apps: Vec::new(),
            lines: Vec::new(),
            pins: Vec::new(),
            recs: Vec::new(),
            view: View::Main,
            page: 0,
            scroll: 0.0,
            query: String::new(),
            edit: Edit::default(),
            search_drag: false,
            found: Found::default(),
            pending_search_keys: None,
            app_index: Vec::new(),
            app_uses: Vec::new(),
            usage: Vec::new(),
            settings: Vec::new(),
            settings_index: Vec::new(),
            hover: Hit::None,
            search_focus: false,
            focus: Hit::None,
            shown: false,
            menu_open: false,
            prev_fg: HWND::default(),
            dirty: true,
            loaded: false,
        })
    }

    pub fn bitmap(&self, img: &shelldata::Image) -> Option<ID2D1Bitmap1> {
        self.surface.bitmap(img.width, img.height, &img.to_premultiplied_bgra()).ok()
    }

    pub fn set_apps(&mut self, apps: Vec<App>, recents: Vec<Recent>, now: i64) {
        self.apps = apps
            .into_iter()
            .map(|a| AppEntry {
                icon: a.icon.as_ref().and_then(|i| self.bitmap(i)),
                name: a.name,
                aumid: a.aumid,
                installed: a.installed,
            })
            .collect();
        let names: Vec<&str> = self.apps.iter().map(|a| a.name.as_str()).collect();
        self.lines = model::all_apps_lines(&names);
        self.app_index = self.apps.iter().map(|a| crate::search::Entry::new(&a.name, &crate::sources::app_keywords(&a.aumid))).collect();
        self.refresh_uses();
        // Recommended: apps installed in the last 7 days and recent files, newest first.
        let mut recs: Vec<(i64, RecEntry)> = Vec::new();
        for a in &self.apps {
            if let Some(t) = a.installed.filter(|t| now - t < 7 * 86400 && now >= *t) {
                recs.push((t, RecEntry {
                    title: a.name.clone(),
                    subtitle: self.s.recently_added.into(),
                    icon: a.icon.clone(),
                    kind: RecKind::App(a.aumid.clone()),
                }));
            }
        }
        for f in recents {
            recs.push((f.when, RecEntry {
                subtitle: model::ago(now - f.when, self.s.french),
                icon: f.icon.as_ref().and_then(|i| self.bitmap(i)),
                title: f.name,
                kind: RecKind::File(f.link),
            }));
        }
        recs.sort_by(|a, b| b.0.cmp(&a.0));
        self.recs = recs.into_iter().take(L::REC_COLS * L::REC_ROWS).map(|(_, e)| e).collect();
        self.loaded = true;
        self.dirty = true;
    }

    /// Indices into `apps` of the pins whose app is installed, in pin order.
    pub fn pinned(&self) -> Vec<usize> {
        self.pins
            .iter()
            .filter_map(|p| self.apps.iter().position(|a| a.aumid.eq_ignore_ascii_case(p)))
            .collect()
    }

    /// Hover or keyboard focus on `h`.
    fn hot(&self, h: Hit) -> bool {
        self.hover == h || self.focus == h
    }

    fn tiles_on_page(&self) -> usize {
        self.pinned().len().saturating_sub(self.page * L::PER_PAGE).min(L::PER_PAGE)
    }

    /// Tab order of the current view.
    fn tab_stops(&self) -> Vec<Hit> {
        let mut v = vec![Hit::Search];
        match self.view {
            View::Main => {
                v.push(Hit::AllApps);
                if self.tiles_on_page() > 0 {
                    v.push(Hit::Tile(0));
                }
                if !self.recs.is_empty() {
                    v.push(Hit::Rec(0));
                }
            }
            View::AllApps => {
                v.push(Hit::Back);
                if let Some(i) = self.next_app_line(0, 1) {
                    v.push(Hit::Row(i));
                }
            }
            View::Search => {
                for i in 0..self.actions().len() {
                    v.push(Hit::Action(i));
                }
            }
        }
        v.push(Hit::User);
        v.push(Hit::Power);
        v
    }

    pub fn tab(&mut self, back: bool) {
        let stops = self.tab_stops();
        let cur = stops.iter().position(|h| *h == self.focus).or_else(|| stops.iter().position(|h| std::mem::discriminant(h) == std::mem::discriminant(&self.focus)));
        let n = stops.len();
        let next = match (cur, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        self.focus = stops[next];
        self.search_focus = self.focus == Hit::Search;
        self.dirty = true;
    }

    /// The next App line from `from` (included) in direction `dir`.
    fn next_app_line(&self, from: usize, dir: isize) -> Option<usize> {
        let mut i = from as isize;
        while i >= 0 && (i as usize) < self.lines.len() {
            if matches!(self.lines[i as usize], Line::App(_)) {
                return Some(i as usize);
            }
            i += dir;
        }
        None
    }

    /// Arrow keys: dx, dy in -1..1.
    pub fn arrow(&mut self, dx: isize, dy: isize) {
        let tiles = self.tiles_on_page() as isize;
        let recs = self.recs.len() as isize;
        let cols = L::COLS as isize;
        self.focus = match (self.view, self.focus) {
            (View::Main, Hit::Tile(i)) => {
                let i = i as isize;
                let j = i + dx + dy * cols;
                if dy > 0 && j >= tiles && recs > 0 {
                    Hit::Rec(0)
                } else if dy < 0 && j < 0 {
                    Hit::Search
                } else {
                    Hit::Tile(j.clamp(0, tiles - 1) as usize)
                }
            }
            (View::Main, Hit::Rec(i)) => {
                let j = i as isize + dx + dy * 2;
                if j < 0 && tiles > 0 {
                    Hit::Tile((tiles - 1) as usize)
                } else {
                    Hit::Rec(j.clamp(0, recs - 1) as usize)
                }
            }
            (View::Main, Hit::Search | Hit::None | Hit::AllApps) if dy > 0 && tiles > 0 => Hit::Tile(0),
            (View::AllApps, Hit::Row(i)) => {
                let next = if dy > 0 { i.checked_add(1).and_then(|k| self.next_app_line(k, 1)) } else { i.checked_sub(1).and_then(|k| self.next_app_line(k, -1)) };
                match next {
                    Some(k) => Hit::Row(k),
                    None if dy < 0 => Hit::Search,
                    None => Hit::Row(i),
                }
            }
            (View::AllApps, Hit::Search | Hit::None | Hit::Back) if dy > 0 => self.next_app_line(0, 1).map_or(Hit::None, Hit::Row),
            (_, f) => f,
        };
        self.search_focus = matches!(self.focus, Hit::Search | Hit::None);
        if let (View::AllApps, Hit::Row(i)) = (self.view, self.focus) {
            self.scroll_into_view(i);
        }
        self.dirty = true;
    }

    fn scroll_into_view(&mut self, line: usize) {
        if let Some(&(top, h, _)) = self.line_boxes().get(line) {
            if top < self.scroll {
                self.scroll = top;
            } else if top + h > self.scroll + L::LIST.h {
                self.scroll = top + h - L::LIST.h;
            }
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        }
    }

    /// The box of a focusable element, in DIPs, for menus and the focus visual.
    pub fn box_of(&self, h: Hit) -> Option<Rect> {
        Some(match h {
            Hit::Search => L::SEARCH,
            Hit::AllApps => L::header_button(L::PINNED_BASELINE, L::ALL_BTN_W),
            Hit::Back => L::header_button(L::PINNED_BASELINE, L::BACK_BTN_W),
            Hit::Tile(i) => {
                let t = L::tile(i);
                Rect::new(t.x + 2.0, t.y + 2.0, t.w - 4.0, t.h - 4.0)
            }
            Hit::Rec(i) => L::rec(i),
            Hit::Action(i) => L::action(i),
            Hit::Row(i) if self.view == View::Search => self.result_box(i)?,
            Hit::Row(i) => {
                let (top, _, _) = *self.line_boxes().get(i)?;
                let y = L::LIST.y + top - self.scroll;
                Rect::new(L::LIST_X - 12.0, y + 2.0, L::W - 2.0 * (L::LIST_X - 12.0), L::ROW_H - 4.0)
            }
            Hit::User => L::USER,
            Hit::Power => L::POWER,
            Hit::PageDot(_) | Hit::None => return None,
        })
    }

    pub fn pages(&self) -> usize {
        L::pages(self.pinned().len())
    }

    /// The app under a tile of the current page.
    pub fn tile_app(&self, slot: usize) -> Option<usize> {
        self.pinned().get(self.page * L::PER_PAGE + slot).copied()
    }

    /// (top, height, line) of All apps' lines, before scrolling.
    pub fn line_boxes(&self) -> Vec<(f32, f32, usize)> {
        let mut y = 0.0;
        let mut out = Vec::with_capacity(self.lines.len());
        for (i, l) in self.lines.iter().enumerate() {
            let h = if matches!(l, Line::Letter(_)) { L::LETTER_H } else { L::ROW_H };
            out.push((y, h, i));
            y += h;
        }
        out
    }

    pub fn max_scroll(&self) -> f32 {
        let content = match self.view {
            View::AllApps => self.line_boxes().last().map(|(y, h, _)| y + h).unwrap_or(0.0),
            View::Search => return self.results_max_scroll(),
            View::Main => 0.0,
        };
        (content - L::LIST.h).max(0.0)
    }

    /// The app of a list row in All apps or Search.
    pub fn row_app(&self, row: usize) -> Option<usize> {
        match self.view {
            View::AllApps => match self.lines.get(row)? {
                Line::App(i) => Some(*i),
                Line::Letter(_) => None,
            },
            View::Search => match self.found.rows.get(row)? {
                crate::search::Row::Item(h) if h.kind == crate::search::Kind::App => Some(h.index),
                _ => None,
            },
            View::Main => None,
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        if self.view == View::Main {
            let on_page = self.pinned().len().saturating_sub(self.page * L::PER_PAGE).min(L::PER_PAGE);
            return L::hit_main(x, y, on_page, self.pages(), self.recs.len());
        }
        if L::SEARCH.contains(x, y) {
            return Hit::Search;
        }
        if self.view == View::Search {
            return self.hit_results(x, y).unwrap_or_else(|| L::hit_band(x, y));
        }
        if self.view == View::AllApps && L::header_button(L::PINNED_BASELINE, L::BACK_BTN_W).contains(x, y) {
            return Hit::Back;
        }
        if L::LIST.contains(x, y) {
            let ly = y - L::LIST.y + self.scroll;
            for (top, h, i) in self.line_boxes() {
                if ly >= top && ly < top + h {
                    return if matches!(self.lines[i], Line::App(_)) { Hit::Row(i) } else { Hit::None };
                }
            }
            return Hit::None;
        }
        L::hit_band(x, y)
    }

    /// Back to the main view, as Start opens every time.
    pub fn reset(&mut self) {
        if self.view != View::Main || self.page != 0 || !self.query.is_empty() || self.hover != Hit::None || self.search_focus || self.focus != Hit::None {
            self.search_focus = false;
            self.focus = Hit::None;
            self.view = View::Main;
            self.page = 0;
            self.scroll = 0.0;
            self.query.clear();
            self.edit = Edit::default();
            self.search_offset = 0.0;
            self.found.clear();
            self.hover = Hit::None;
            self.dirty = true;
        }
    }

    // Drawing.

    fn color(&self, c: Color) -> &ID2D1Brush {
        // SAFETY: recolours our own brush.
        unsafe { self.brush.SetColor(&c) };
        &self.base
    }

    fn fill(&self, b: &Rect, radius: f32, c: Color) {
        let rr = D2D1_ROUNDED_RECT { rect: r(b), radiusX: radius, radiusY: radius };
        // SAFETY: drawing inside BeginDraw/EndDraw.
        unsafe { self.surface.ctx.FillRoundedRectangle(&rr, self.color(c)) };
    }

    fn stroke(&self, b: &Rect, radius: f32, c: Color) {
        let inset = Rect::new(b.x + 0.5, b.y + 0.5, b.w - 1.0, b.h - 1.0);
        let rr = D2D1_ROUNDED_RECT { rect: r(&inset), radiusX: radius, radiusY: radius };
        // SAFETY: as in fill.
        unsafe { self.surface.ctx.DrawRoundedRectangle(&rr, self.color(c), 1.0, None) };
    }

    fn txt(&self, f: &IDWriteTextFormat, s: &str, x0: f32, x1: f32, baseline: f32, c: Color) {
        let b = self.color(c).clone();
        text::draw(&self.surface.ctx, &self.text, f, s, x0, x1, baseline, &b, false);
    }

    /// Centred text wrapped on up to two lines in a box, trimmed with an ellipsis.
    fn wrapped(&self, s: &str, x: f32, y: f32, w: f32, h: f32, c: Color) {
        let t: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: a layout drawn inside BeginDraw.
        unsafe {
            let Ok(l) = self.text.factory.CreateTextLayout(&t, &self.label_wrap, w, h) else { return };
            self.surface.ctx.DrawTextLayout(Vector2 { X: x, Y: y }, &l, self.color(c), D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    }

    fn icon(&self, bmp: &Option<ID2D1Bitmap1>, x: f32, y: f32, size: f32) {
        if let Some(b) = bmp {
            let rect = D2D_RECT_F { left: x, top: y, right: x + size, bottom: y + size };
            // SAFETY: as in fill.
            unsafe {
                self.surface.ctx.DrawBitmap(b, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None)
            };
        }
    }

    pub fn draw(&mut self) -> Result<()> {
        self.surface.begin();
        render::style::paint_base(&self.surface.ctx, render::style::Part::Start);
        // SAFETY: inside BeginDraw.
        unsafe { self.surface.ctx.SetTransform(&Matrix3x2::scale(self.scale, self.scale)) };
        self.draw_search();
        match self.view {
            View::Main => self.draw_main(),
            View::AllApps => self.draw_all_apps(),
            View::Search => self.draw_results(),
        }
        self.draw_band();
        if self.focus != Hit::Search {
            if let Some(b) = self.box_of(self.focus) {
                let outer = Rect::new(b.x - 1.0, b.y - 1.0, b.w + 2.0, b.h + 2.0);
                let rr = D2D1_ROUNDED_RECT { rect: r(&outer), radiusX: 5.0, radiusY: 5.0 };
                // SAFETY: inside BeginDraw.
                unsafe { self.surface.ctx.DrawRoundedRectangle(&rr, self.color(WHITE), 2.0, None) };
            }
        }
        self.surface.end()?;
        self.dirty = false;
        self.pending_search_keys = None;
        Ok(())
    }

    fn header_button(&self, baseline: f32, width: f32, label: &str, back: bool, hot: bool) {
        let b = L::header_button(baseline, width);
        self.fill(&b, 4.0, rgba(255, 255, 255, if hot { 0.083 } else { 0.0605 }));
        self.stroke(&b, 4.0, STROKE);
        if back {
            self.txt(&self.glyph_small, GLYPH_CHEVRON_L, b.x + 6.0, b.x + 22.0, baseline - 1.0, WHITE);
            self.txt(&self.label_left, label, b.x + 24.0, b.right() - 4.0, baseline, WHITE);
        } else {
            self.txt(&self.label_left, label, b.x + 10.0, b.right() - 18.0, baseline, WHITE);
            self.txt(&self.glyph_small, GLYPH_CHEVRON_R, b.right() - 22.0, b.right() - 6.0, baseline - 1.0, WHITE);
        }
    }

    fn draw_main(&self) {
        let s = self.s;
        self.txt(&self.header, s.pinned, L::HEADER_X, 400.0, L::PINNED_BASELINE, WHITE);
        self.header_button(L::PINNED_BASELINE, L::ALL_BTN_W, s.all, false, self.hot(Hit::AllApps));
        let pinned = self.pinned();
        let first = self.page * L::PER_PAGE;
        for (slot, &ai) in pinned.iter().skip(first).take(L::PER_PAGE).enumerate() {
            let t = L::tile(slot);
            if self.hot(Hit::Tile(slot)) {
                self.fill(&Rect::new(t.x + 2.0, t.y + 2.0, t.w - 4.0, t.h - 4.0), 4.0, HOVER);
            }
            let a = &self.apps[ai];
            self.icon(&a.icon, t.x + (t.w - L::TILE_ICON) / 2.0, t.y + L::TILE_ICON_TOP, L::TILE_ICON);
            self.wrapped(&a.name, t.x + 4.0, t.y + L::TILE_LABEL_TOP, t.w - 8.0, 32.0, WHITE);
        }
        let pages = self.pages();
        if pages > 1 {
            for p in 0..pages {
                let (cx, cy) = L::page_dot(p, pages);
                let (rad, c) = if p == self.page { (3.0, SECONDARY) } else { (2.0, TERTIARY) };
                let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: rad, radiusY: rad };
                // SAFETY: as in fill.
                unsafe { self.surface.ctx.FillEllipse(&e, self.color(c)) };
            }
        }
        self.txt(&self.header, s.recommended, L::HEADER_X, 400.0, L::REC_BASELINE, WHITE);
        if self.recs.is_empty() && self.loaded {
            let f = &self.label_left;
            // Two lines, as Windows wraps its empty-state text.
            let words: Vec<&str> = s.empty_rec.split(' ').collect();
            let half = words.len() / 2;
            self.txt(f, &words[..half].join(" "), L::HEADER_X, L::W - 52.0, L::REC_Y + 22.0, SECONDARY);
            self.txt(f, &words[half..].join(" "), L::HEADER_X, L::W - 52.0, L::REC_Y + 40.0, SECONDARY);
        }
        for (i, e) in self.recs.iter().enumerate() {
            let b = L::rec(i);
            if self.hot(Hit::Rec(i)) {
                self.fill(&b, 4.0, HOVER);
            }
            let iy = b.y + (b.h - L::REC_ICON) / 2.0;
            if e.icon.is_some() {
                self.icon(&e.icon, b.x + 12.0, iy, L::REC_ICON);
            } else {
                self.txt(&self.glyph, GLYPH_DOC, b.x + 12.0, b.x + 44.0, iy + 23.0, SECONDARY);
            }
            self.txt(&self.label_left, &e.title, b.x + 56.0, b.right() - 8.0, b.y + 26.0, WHITE);
            self.txt(&self.label_left, &e.subtitle, b.x + 56.0, b.right() - 8.0, b.y + 42.0, SECONDARY);
        }
    }

    fn list_clip<F: FnOnce()>(&self, f: F) {
        // SAFETY: a clip pushed and popped around the list.
        unsafe { self.surface.ctx.PushAxisAlignedClip(&r(&L::LIST), D2D1_ANTIALIAS_MODE_ALIASED) };
        f();
        unsafe { self.surface.ctx.PopAxisAlignedClip() };
    }

    fn row(&self, top: f32, ai: usize, hot: bool, selected: bool) {
        let b = Rect::new(L::LIST_X - 12.0, top + 2.0, L::W - 2.0 * (L::LIST_X - 12.0), L::ROW_H - 4.0);
        if hot || selected {
            self.fill(&b, 4.0, if selected { rgba(255, 255, 255, 0.083) } else { HOVER });
        }
        let a = &self.apps[ai];
        self.icon(&a.icon, L::LIST_X, top + (L::ROW_H - L::LIST_ICON) / 2.0, L::LIST_ICON);
        self.txt(&self.label_left, &a.name, L::LIST_X + 40.0, L::W - 60.0, top + 25.0, WHITE);
    }

    fn draw_all_apps(&self) {
        let s = self.s;
        self.txt(&self.header, s.all_apps, L::HEADER_X, 400.0, L::PINNED_BASELINE, WHITE);
        self.header_button(L::PINNED_BASELINE, L::BACK_BTN_W, s.back, true, self.hot(Hit::Back));
        self.list_clip(|| {
            for (top, h, i) in self.line_boxes() {
                let y = L::LIST.y + top - self.scroll;
                if y + h < L::LIST.y || y > L::LIST.bottom() {
                    continue;
                }
                match self.lines[i] {
                    Line::Letter(c) => self.txt(&self.header, &c.to_string(), L::LIST_X, 200.0, y + 24.0, WHITE),
                    Line::App(ai) => self.row(y, ai, self.hot(Hit::Row(i)), false),
                }
            }
        });
    }

    fn draw_band(&self) {
        let b = L::BAND;
        // SAFETY: as in fill.
        unsafe {
            let band = D2D_RECT_F { left: 0.0, top: b.y, right: L::W, bottom: L::H };
            self.surface.ctx.FillRectangle(&band, self.color(rgba(0, 0, 0, 0.22)));
            let line = D2D_RECT_F { left: 0.0, top: b.y, right: L::W, bottom: b.y + 1.0 };
            self.surface.ctx.FillRectangle(&line, self.color(rgba(255, 255, 255, 0.06)));
        }
        if self.hot(Hit::User) {
            self.fill(&L::USER, 4.0, HOVER);
        }
        if self.hot(Hit::Power) {
            self.fill(&L::POWER, 4.0, HOVER);
        }
        let u = L::USER;
        let (cx, cy) = (u.x + 11.0 + L::AVATAR / 2.0, u.y + u.h / 2.0);
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: L::AVATAR / 2.0, radiusY: L::AVATAR / 2.0 };
        // SAFETY: as in fill.
        unsafe { self.surface.ctx.FillEllipse(&e, self.color(rgba(255, 255, 255, 0.10))) };
        self.txt(&self.glyph, GLYPH_USER, cx - 12.0, cx + 12.0, cy + 8.0, SECONDARY);
        self.txt(&self.label_left, &self.user, u.x + 56.0, u.right() - 4.0, cy + 4.0, WHITE);
        let p = L::POWER;
        self.txt(&self.glyph, GLYPH_POWER, p.x, p.right(), p.y + p.h / 2.0 + 8.0, WHITE);
    }
}
