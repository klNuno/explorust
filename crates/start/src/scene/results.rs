//! The search view of Start, as Windows 11 lays it out: the results on the left (best match, then
//! groups by kind) and the preview of the selected result on the right with its actions.

use super::{HOVER, SECONDARY, Start, TERTIARY, WHITE, r};
use crate::layout::{self as L, Hit, Rect};
use crate::search::{self, Kind, Row};
use render::rgba;
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;

const GLYPH_SETTINGS: &str = "\u{E713}";
const GLYPH_CALC: &str = "\u{E8EF}";
const GLYPH_FOLDER: &str = "\u{E8B7}";
const GLYPH_DOC: &str = "\u{E8A5}";
const GLYPH_OPEN: &str = "\u{E8A7}";
const GLYPH_ADMIN: &str = "\u{E7EF}";
const GLYPH_LOCATION: &str = "\u{ED43}";
const GLYPH_COPY: &str = "\u{E8C8}";
const GLYPH_PIN: &str = "\u{E718}";
const GLYPH_UNPIN: &str = "\u{E77A}";

pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub type_text: String,
    pub folder: bool,
    pub icon: Option<ID2D1Bitmap1>,
}

/// The results of the current query.
#[derive(Default)]
pub struct Found {
    pub rows: Vec<Row>,
    /// The selected row (an Item), shown in the preview.
    pub sel: Option<usize>,
    pub apps: Vec<(usize, u32)>,
    pub settings: Vec<(usize, u32)>,
    /// (expression, result) when the query is a sum.
    pub calc: Option<(String, String)>,
    pub files: Vec<FileEntry>,
    /// Bumped on every query; file batches of an older one are dropped.
    pub generation: u64,
    /// No batch of the current generation has arrived yet (the old files are still shown).
    pub files_stale: bool,
    pub files_done: bool,
}

impl Found {
    pub fn clear(&mut self) {
        let generation = self.generation;
        *self = Found { generation, ..Default::default() };
    }
}

/// What an action of the preview or the context menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Open,
    RunAs,
    Location,
    CopyPath,
    Pin,
    Unpin,
    Copy,
}

impl Start {
    /// Draws the search view once while cloaked, so the first key does not pay for font loading.
    pub fn warm(&mut self) {
        if self.shown {
            return;
        }
        let t = std::time::Instant::now();
        self.set_query("a".into());
        let _ = self.draw();
        self.set_query(String::new());
        crate::log::line(&format!("search: view warmed in {:.1} ms", t.elapsed().as_secs_f32() * 1000.0));
    }

    pub fn refresh_uses(&mut self) {
        self.app_uses = self.apps.iter().map(|a| self.usage.iter().find(|(id, _)| id.eq_ignore_ascii_case(&a.aumid)).map_or(0, |u| u.1)).collect();
    }

    /// Counts one more launch of an app (ranks it higher next time); returns the table to save.
    pub fn count_use(&mut self, aumid: &str) -> Vec<(String, u32)> {
        match self.usage.iter_mut().find(|(id, _)| id.eq_ignore_ascii_case(aumid)) {
            Some(u) => u.1 += 1,
            None => self.usage.push((aumid.to_string(), 1)),
        }
        self.usage.sort_by(|a, b| b.1.cmp(&a.1));
        self.refresh_uses();
        self.usage.clone()
    }

    /// Ranks apps and settings and runs the calculator for `q`, all in memory; files come later.
    /// Returns the new generation for the index query.
    pub fn set_query(&mut self, q: String) -> u64 {
        let uses = &self.app_uses;
        let apps = search::rank(&self.app_index, |i| search::use_bonus(uses.get(i).copied().unwrap_or(0)), &q, 12);
        let settings = search::rank(&self.settings_index, |_| 0, &q, 12);
        let calc = search::calc(&q).map(|v| (q.trim().to_string(), search::format_number(v, self.s.french)));
        let f = &mut self.found;
        f.generation += 1;
        f.apps = apps;
        f.settings = settings;
        f.calc = calc;
        f.files_stale = true;
        f.files_done = false;
        // Keep the files of the previous query that still match, until the index answers.
        let words: Vec<String> = q.split_whitespace().map(search::fold).collect();
        f.files.retain(|e| {
            let n = search::fold(&e.name);
            words.iter().all(|w| n.split(|c: char| !c.is_alphanumeric()).any(|p| p.starts_with(w.as_str())))
        });
        self.query = q;
        self.focus = Hit::None;
        self.scroll = 0.0;
        self.view = if self.query.is_empty() { View::Main } else { View::Search };
        if self.query.is_empty() {
            self.found.clear();
        }
        self.rebuild_rows(None);
        self.dirty = true;
        self.found.generation
    }

    /// Adds a batch of files from the index; false when it belongs to an older query.
    pub fn add_files(&mut self, generation: u64, files: Vec<FileEntry>, done: bool) -> bool {
        if generation != self.found.generation || self.view != View::Search {
            return false;
        }
        let keep = self.selected();
        if self.found.files_stale {
            self.found.files.clear();
            self.found.files_stale = false;
        }
        self.found.files.extend(files);
        self.found.files_done = done;
        self.rebuild_rows(keep);
        self.dirty = true;
        true
    }

    fn rebuild_rows(&mut self, keep: Option<search::Hit>) {
        let f = &self.found;
        let folders: Vec<usize> = (0..f.files.len()).filter(|&i| f.files[i].folder).collect();
        let files: Vec<usize> = (0..f.files.len()).filter(|&i| !f.files[i].folder).collect();
        let rows = search::rows(f.calc.is_some(), &f.apps, &f.settings, &folders, &files);
        let sel = keep
            .and_then(|k| rows.iter().position(|r| matches!(r, Row::Item(h) if h.kind == k.kind && h.index == k.index)))
            .or_else(|| search::next_item(&rows, None, 1));
        self.found.rows = rows;
        self.found.sel = sel;
    }

    /// The selected result (the best match until the arrows move).
    pub fn selected(&self) -> Option<search::Hit> {
        match self.found.rows.get(self.found.sel?)? {
            Row::Item(h) => Some(*h),
            _ => None,
        }
    }

    pub fn move_selection(&mut self, dir: isize) {
        let next = search::next_item(&self.found.rows, self.found.sel, dir);
        if dir < 0 && next == self.found.sel {
            // Up from the first result goes back to the search box.
            self.focus = Hit::Search;
        }
        self.found.sel = next;
        if let Some(i) = next {
            let boxes = self.result_boxes();
            if let Some(&(top, h)) = boxes.get(i) {
                let first = if i == 2 || i == 1 { 0.0 } else { top };
                if first < self.scroll {
                    self.scroll = first;
                } else if top + h > self.scroll + L::RESULTS.h {
                    self.scroll = top + h - L::RESULTS.h;
                }
            }
        }
        self.dirty = true;
    }

    /// (top, height) of every row before scrolling.
    fn result_boxes(&self) -> Vec<(f32, f32)> {
        let mut y = 0.0;
        let mut out = Vec::with_capacity(self.found.rows.len());
        for (i, row) in self.found.rows.iter().enumerate() {
            let h = match row {
                Row::BestHeader | Row::Header(_) => L::RES_HEADER_H,
                Row::Item(_) if i == 1 && self.found.rows[0] == Row::BestHeader => L::RES_BEST_H,
                Row::Item(_) => L::RES_ROW_H,
            };
            out.push((y, h));
            y += h;
        }
        out
    }

    pub fn results_max_scroll(&self) -> f32 {
        let total = self.result_boxes().last().map_or(0.0, |(y, h)| y + h);
        (total - L::RESULTS.h).max(0.0)
    }

    /// The on-screen box of result row `i`.
    pub fn result_box(&self, i: usize) -> Option<Rect> {
        let (top, h) = *self.result_boxes().get(i)?;
        Some(Rect::new(L::RESULTS.x, L::RESULTS.y + top - self.scroll, L::RESULTS.w, h))
    }

    pub fn hit_results(&self, x: f32, y: f32) -> Option<Hit> {
        if L::RESULTS.contains(x, y) {
            let ly = y - L::RESULTS.y + self.scroll;
            for (i, (top, h)) in self.result_boxes().into_iter().enumerate() {
                if ly >= top && ly < top + h && matches!(self.found.rows[i], Row::Item(_)) {
                    return Some(Hit::Row(i));
                }
            }
            return Some(Hit::None);
        }
        (0..self.actions().len()).map(Hit::Action).find(|h| matches!(h, Hit::Action(i) if L::action(*i).contains(x, y)))
    }

    pub fn is_pinned(&self, aumid: &str) -> bool {
        self.pins.iter().any(|p| p.eq_ignore_ascii_case(aumid))
    }

    /// The actions of a result, in Windows' order.
    pub fn actions_of(&self, h: search::Hit) -> Vec<Act> {
        match h.kind {
            Kind::Calc => vec![Act::Copy, Act::Open],
            Kind::App => {
                let Some(a) = self.apps.get(h.index) else { return Vec::new() };
                let mut v = vec![Act::Open];
                if !a.aumid.contains('!') {
                    v.push(Act::RunAs);
                    v.push(Act::Location);
                }
                v.push(if self.is_pinned(&a.aumid) { Act::Unpin } else { Act::Pin });
                v
            }
            Kind::Setting => vec![Act::Open],
            Kind::File => vec![Act::Open, Act::Location, Act::CopyPath],
            Kind::Folder => vec![Act::Open, Act::Location, Act::CopyPath],
        }
    }

    pub fn actions(&self) -> Vec<Act> {
        self.selected().map_or(Vec::new(), |h| self.actions_of(h))
    }

    pub fn act_label(&self, a: Act) -> &'static str {
        let s = self.s;
        match a {
            Act::Open => s.open,
            Act::RunAs => s.run_as,
            Act::Location => s.location,
            Act::CopyPath => s.copy_path,
            Act::Pin => s.pin,
            Act::Unpin => s.unpin,
            Act::Copy => s.copy,
        }
    }

    fn kind_header(&self, k: Kind) -> &'static str {
        match k {
            Kind::Calc => self.s.kind_calc,
            Kind::App => self.s.results,
            Kind::Setting => self.s.settings,
            Kind::Folder => self.s.folders,
            Kind::File => self.s.documents,
        }
    }

    fn kind_label(&self, k: Kind) -> &'static str {
        match k {
            Kind::Calc => self.s.kind_calc,
            Kind::App => self.s.kind_app,
            Kind::Setting => self.s.kind_setting,
            Kind::Folder => self.s.kind_folder,
            Kind::File => self.s.kind_file,
        }
    }

    /// Title, subtitle, icon bitmap or glyph of a result.
    fn describe(&self, h: search::Hit) -> (String, String, Option<ID2D1Bitmap1>, &'static str) {
        match h.kind {
            Kind::Calc => {
                let (e, v) = self.found.calc.clone().unwrap_or_default();
                (format!("= {v}"), e, None, GLYPH_CALC)
            }
            Kind::App => {
                let a = &self.apps[h.index];
                (a.name.clone(), self.s.kind_app.into(), a.icon.clone(), "")
            }
            Kind::Setting => (self.settings[h.index].name.clone(), self.s.kind_setting.into(), None, GLYPH_SETTINGS),
            Kind::File | Kind::Folder => {
                let f = &self.found.files[h.index];
                let glyph = if f.folder { GLYPH_FOLDER } else { GLYPH_DOC };
                (f.name.clone(), f.path.clone(), f.icon.clone(), glyph)
            }
        }
    }

    pub fn draw_results(&self) {
        let rows = &self.found.rows;
        if rows.is_empty() {
            let msg = if self.found.files_done { self.s.no_results } else { self.s.searching };
            self.txt(&self.label_left, msg, L::RESULTS.x + 12.0, L::RESULTS.right(), L::RESULTS.y + 24.0, SECONDARY);
            return;
        }
        // SAFETY: a clip pushed and popped around the list, inside BeginDraw.
        unsafe { self.surface.ctx.PushAxisAlignedClip(&r(&L::RESULTS), D2D1_ANTIALIAS_MODE_ALIASED) };
        for (i, (top, h)) in self.result_boxes().into_iter().enumerate() {
            let y = L::RESULTS.y + top - self.scroll;
            if y + h < L::RESULTS.y || y > L::RESULTS.bottom() {
                continue;
            }
            let b = Rect::new(L::RESULTS.x, y, L::RESULTS.w, h);
            match rows[i] {
                Row::BestHeader => self.txt(&self.header, self.s.best_match, b.x + 12.0, b.right(), y + 24.0, WHITE),
                Row::Header(k) => self.txt(&self.header, self.kind_header(k), b.x + 12.0, b.right(), y + 24.0, WHITE),
                Row::Item(hit) => self.draw_result(i, hit, b, h == L::RES_BEST_H),
            }
        }
        // SAFETY: pairs the push above.
        unsafe { self.surface.ctx.PopAxisAlignedClip() };
        if let Some(h) = self.selected() {
            self.draw_preview(h);
        }
    }

    fn draw_result(&self, i: usize, hit: search::Hit, b: Rect, best: bool) {
        let sel = self.found.sel == Some(i);
        let inner = Rect::new(b.x, b.y + 2.0, b.w, b.h - 4.0);
        if sel {
            self.fill(&inner, 4.0, rgba(255, 255, 255, 0.083));
            // Windows' selection pill on the left edge.
            self.fill(&Rect::new(inner.x, inner.y + inner.h / 2.0 - 8.0, 3.0, 16.0), 1.5, rgba(0x99, 0xEB, 0xFF, 1.0));
        } else if self.hover == Hit::Row(i) {
            self.fill(&inner, 4.0, HOVER);
        }
        let (title, sub, icon, glyph) = self.describe(hit);
        let size = if best { 32.0 } else { L::LIST_ICON };
        let ix = b.x + 12.0;
        let iy = b.y + (b.h - size) / 2.0;
        if icon.is_some() {
            self.icon(&icon, ix, iy, size);
        } else {
            self.txt(&self.glyph, glyph, ix, ix + size, iy + size / 2.0 + 8.0, SECONDARY);
        }
        let tx = ix + size + 12.0;
        if best {
            let sub = if hit.kind == Kind::File || hit.kind == Kind::Folder { self.kind_label(hit.kind).to_string() } else { sub };
            self.txt(&self.body, &title, tx, b.right() - 8.0, b.y + 28.0, WHITE);
            self.txt(&self.label_left, &sub, tx, b.right() - 8.0, b.y + 46.0, SECONDARY);
        } else {
            self.txt(&self.label_left, &title, tx, b.right() - 8.0, b.y + 25.0, WHITE);
        }
    }

    fn draw_preview(&self, h: search::Hit) {
        let p = L::PREVIEW;
        self.fill(&p, 8.0, rgba(255, 255, 255, 0.04));
        self.stroke(&p, 8.0, rgba(255, 255, 255, 0.06));
        let (title, sub, icon, glyph) = self.describe(h);
        let size = L::PREVIEW_ICON;
        let ix = p.x + (p.w - size) / 2.0;
        let iy = p.y + 28.0;
        if icon.is_some() {
            self.icon(&icon, ix, iy, size);
        } else {
            self.txt(&self.glyph_big, glyph, ix - 8.0, ix + size + 8.0, iy + size / 2.0 + 20.0, WHITE);
        }
        self.txt(&self.title, &title, p.x + 12.0, p.right() - 12.0, iy + size + 36.0, WHITE);
        let kind = match h.kind {
            Kind::File | Kind::Folder => Some(&self.found.files[h.index].type_text).filter(|t| !t.is_empty()).cloned(),
            _ => None,
        }
        .unwrap_or_else(|| self.kind_label(h.kind).to_string());
        self.txt(&self.center, &kind, p.x + 12.0, p.right() - 12.0, iy + size + 58.0, SECONDARY);
        if matches!(h.kind, Kind::File | Kind::Folder | Kind::Calc) {
            self.txt(&self.center, &sub, p.x + 12.0, p.right() - 12.0, iy + size + 78.0, TERTIARY);
        }
        let sep = Rect::new(p.x + 12.0, L::ACTIONS_Y - 12.0, p.w - 24.0, 1.0);
        self.fill(&sep, 0.0, rgba(255, 255, 255, 0.08));
        for (i, a) in self.actions_of(h).into_iter().enumerate() {
            let b = L::action(i);
            if self.hot(Hit::Action(i)) {
                self.fill(&b, 4.0, HOVER);
            }
            let g = match a {
                Act::Open => GLYPH_OPEN,
                Act::RunAs => GLYPH_ADMIN,
                Act::Location => GLYPH_LOCATION,
                Act::CopyPath | Act::Copy => GLYPH_COPY,
                Act::Pin => GLYPH_PIN,
                Act::Unpin => GLYPH_UNPIN,
            };
            self.txt(&self.glyph, g, b.x + 6.0, b.x + 34.0, b.y + b.h / 2.0 + 8.0, WHITE);
            self.txt(&self.label_left, self.act_label(a), b.x + 44.0, b.right() - 8.0, b.y + b.h / 2.0 + 5.0, WHITE);
        }
        if !self.found.files_done {
            self.txt(&self.label_left, self.s.searching, p.x + 16.0, p.right() - 12.0, p.bottom() - 14.0, TERTIARY);
        }
    }
}

use super::View;
