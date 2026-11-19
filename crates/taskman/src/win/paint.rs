//! Drawing of the three pages: navigation rail, command bar, the grouped process table with
//! heat-tinted cells, the performance tiles and graph, the startup list.

use super::{App, Hit, Page, first_frame, theme};
use crate::model::{History, Section, mb, mbps, pct, rate};
use render::rgba;
use render::text::{Align, Text, draw};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_numerics::Vector2;

pub struct Fonts {
    pub body: IDWriteTextFormat,
    pub right: IDWriteTextFormat,
    pub bold: IDWriteTextFormat,
    pub title: IDWriteTextFormat,
    pub glyph: IDWriteTextFormat,
    pub big: IDWriteTextFormat,
}

impl Fonts {
    pub fn new(t: &Text, k: f32) -> Fonts {
        let f = |fam, size: f32, wt, al| t.format(fam, size * k, wt, al).unwrap();
        Fonts {
            body: f("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Leading),
            right: f("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Trailing),
            bold: f("Segoe UI Variable Text", 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, Align::Leading),
            title: f("Segoe UI Variable Display", 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, Align::Leading),
            glyph: f("Segoe Fluent Icons", 16.0, DWRITE_FONT_WEIGHT_NORMAL, Align::Center),
            big: f("Segoe UI Variable Display", 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, Align::Trailing),
        }
    }
}

fn r(l: f32, t: f32, rr: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: rr, bottom: b }
}

struct B {
    text: ID2D1SolidColorBrush,
    dim: ID2D1SolidColorBrush,
    hover: ID2D1SolidColorBrush,
    sel: ID2D1SolidColorBrush,
    line: ID2D1SolidColorBrush,
    accent: ID2D1SolidColorBrush,
    accent_fill: ID2D1SolidColorBrush,
    heat: ID2D1SolidColorBrush,
    card: ID2D1SolidColorBrush,
}

/// Heat tint of a cell, as Task Manager's yellow to orange scale.
fn heat(b: &ID2D1SolidColorBrush, v: f64, light: bool) {
    let a = (0.08 + (v.clamp(0.0, 1.0) as f32) * 0.5).min(0.6);
    let c = if v > 0.5 { rgba(255, 140, 0, a) } else if light { rgba(255, 210, 0, a) } else { rgba(255, 200, 40, a * 0.8) };
    unsafe { b.SetColor(&c) };
}

impl App {
    pub fn paint(&mut self) {
        let p = theme::palette(self.light);
        self.hits.clear();
        let s = &self.surface;
        let accent = if self.light { rgba(0, 95, 184, 1.0) } else { rgba(96, 205, 255, 1.0) };
        let b = (|| -> windows_core::Result<B> {
            Ok(B {
                text: s.brush(p.text)?,
                dim: s.brush(p.dim)?,
                hover: s.brush(p.hover)?,
                sel: s.brush(if self.light { rgba(0, 0, 0, 0.06) } else { rgba(255, 255, 255, 0.09) })?,
                line: s.brush(if self.light { rgba(0, 0, 0, 0.08) } else { rgba(255, 255, 255, 0.08) })?,
                accent: s.brush(accent)?,
                accent_fill: s.brush(render::Color { a: 0.18, ..accent })?,
                heat: s.brush(rgba(255, 200, 0, 0.1))?,
                card: s.brush(if self.light { rgba(255, 255, 255, 0.5) } else { rgba(255, 255, 255, 0.04) })?,
            })
        })();
        let Ok(b) = b else { return };
        s.begin();
        self.rail(&b);
        match self.page {
            Page::Processes => self.processes(&b),
            Page::Performance => self.performance(&b),
            Page::Startup => self.startup_page(&b),
        }
        let _ = self.surface.end();
        first_frame(self);
    }

    fn button(&mut self, b: &B, x1: f32, glyph: char, label: &str, hit: Hit, enabled: bool) -> f32 {
        let k = self.k;
        let w = (label.chars().count() as f32 * 6.6 + 44.0) * k;
        let (x0, y0, y1) = (x1 - w, 12.0 * k, 44.0 * k);
        let ctx = &self.surface.ctx;
        unsafe {
            if enabled && self.hover == Some(hit) {
                ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r(x0, y0, x1, y1), radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.hover);
            }
        }
        let brush = if enabled { &b.text } else { &b.dim };
        draw(ctx, &self.text, &self.f.glyph, &glyph.to_string(), x0 + 4.0 * k, x0 + 30.0 * k, 34.0 * k, brush, false);
        draw(ctx, &self.text, &self.f.body, label, x0 + 32.0 * k, x1, 32.0 * k, brush, false);
        if enabled {
            self.hits.push((x0, y0, x1, y1, hit));
        }
        x0 - 4.0 * k
    }

    fn rail(&mut self, b: &B) {
        let k = self.k;
        let items = [(Page::Processes, '\u{E71D}'), (Page::Performance, '\u{E9D9}'), (Page::Startup, '\u{E7B5}')];
        for (i, (page, g)) in items.into_iter().enumerate() {
            let (x0, y0) = (4.0 * k, (8.0 + i as f32 * 40.0) * k);
            let (x1, y1) = (x0 + 40.0 * k, y0 + 36.0 * k);
            let ctx = &self.surface.ctx;
            unsafe {
                if self.page == page {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r(x0, y0, x1, y1), radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.sel);
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r(x0, y0 + 10.0 * k, x0 + 3.0 * k, y1 - 10.0 * k), radiusX: 1.5 * k, radiusY: 1.5 * k }, &b.accent);
                } else if self.hover == Some(Hit::Nav(page)) {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r(x0, y0, x1, y1), radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.hover);
                }
            }
            draw(ctx, &self.text, &self.f.glyph, &g.to_string(), x0, x1, y0 + 24.0 * k, &b.text, false);
            self.hits.push((x0, y0, x1, y1, Hit::Nav(page)));
        }
    }

    fn header(&mut self, b: &B, title: &str) -> f32 {
        let k = self.k;
        draw(&self.surface.ctx, &self.text, &self.f.title, title, 64.0 * k, 400.0 * k, 36.0 * k, &b.text, false);
        let w = self.size.0 as f32;
        let label = self.t("Windows Task Manager", "Gestionnaire Windows").to_string();
        self.button(b, w - 12.0 * k, '\u{E8A7}', &label, Hit::RealTaskmgr, true)
    }

    fn processes(&mut self, b: &B) {
        let k = self.k;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let title = self.t("Processes", "Processus").to_string();
        let x = self.header(b, &title);
        let label = self.t("End task", "Fin de tâche").to_string();
        self.button(b, x, '\u{E711}', &label, super::Hit::EndTask, self.selected.is_some());
        // Columns, right-aligned values: CPU, Memory, Disk, Network, GPU.
        let cols = [86.0, 96.0, 86.0, 86.0, 76.0].map(|c| c * k);
        let mut edges = Vec::new();
        let mut xr = w - 16.0 * k;
        for c in cols.iter().rev() {
            edges.push((xr - c, xr));
            xr -= c;
        }
        edges.reverse();
        let name_x = 64.0 * k;
        let t = &self.s.totals;
        let mem_pct = t.mem.last();
        let heads = [
            (pct(t.cpu.last()), self.t("CPU", "Processeur")),
            (format!("{mem_pct:.0}%"), self.t("Memory", "Mémoire")),
            (rate(t.disk.last()), self.t("Disk", "Disque")),
            (mbps(t.net.last()), self.t("Network", "Réseau")),
            (pct(t.gpu.last()), "GPU"),
        ];
        let ctx = &self.surface.ctx;
        let hy = 56.0 * k;
        draw(ctx, &self.text, &self.f.body, self.t("Name", "Nom"), name_x + 8.0 * k, edges[0].0, hy + 34.0 * k, &b.dim, false);
        for (i, (v, l)) in heads.iter().enumerate() {
            let (x0, x1) = edges[i];
            draw(ctx, &self.text, &self.f.big, v, x0, x1 - 8.0 * k, hy + 16.0 * k, &b.text, true);
            draw(ctx, &self.text, &self.f.right, l, x0, x1 - 8.0 * k, hy + 34.0 * k, &b.dim, false);
        }
        unsafe { ctx.FillRectangle(&r(name_x, hy + 44.0 * k, w - 16.0 * k, hy + 45.0 * k), &b.line) };
        let top = hy + 46.0 * k;
        let row_h = 28.0 * k;
        // Visible rows: section headers, groups, expanded children.
        enum Row {
            Section(Section, usize),
            Group(usize),
            Child(usize, u32),
        }
        let mut rows = Vec::new();
        let mut last = None;
        for (gi, g) in self.groups.iter().enumerate() {
            if last != Some(g.section) {
                let n = self.groups.iter().filter(|x| x.section == g.section).count();
                rows.push(Row::Section(g.section, n));
                last = Some(g.section);
            }
            rows.push(Row::Group(gi));
            let lead = self.s.procs[g.lead].pid;
            if g.pids.len() > 1 && self.expanded.contains(&lead) {
                rows.extend(g.pids.iter().map(|&p| Row::Child(gi, p)));
            }
        }
        let max_scroll = (rows.len() as f32 * row_h - (h - top)).max(0.0);
        self.scroll = self.scroll.min(max_scroll);
        let first = (self.scroll / row_h) as usize;
        let ncpu = 100.0;
        let mem_total = self.s.totals.mem_total.max(1) as f64;
        let ctx = self.surface.ctx.clone();
        unsafe { ctx.PushAxisAlignedClip(&r(0.0, top, w, h), D2D1_ANTIALIAS_MODE_ALIASED) };
        for (ri, row) in rows.iter().enumerate().skip(first) {
            let y0 = top + ri as f32 * row_h - self.scroll;
            if y0 > h {
                break;
            }
            let base = y0 + 19.0 * k;
            match row {
                Row::Section(sec, n) => {
                    let name = match sec {
                        Section::Apps => self.t("Apps", "Applications"),
                        Section::Background => self.t("Background processes", "Processus en arrière-plan"),
                        Section::Windows => self.t("Windows processes", "Processus Windows"),
                    };
                    draw(&ctx, &self.text, &self.f.bold, &format!("{name} ({n})"), name_x + 8.0 * k, w, base, &b.text, false);
                }
                Row::Group(gi) | Row::Child(gi, _) => {
                    let g = &self.groups[*gi];
                    let lead = self.s.procs[g.lead].pid;
                    let child = if let Row::Child(_, pid) = row { Some(*pid) } else { None };
                    let (name, vals) = match child {
                        None => (
                            if g.pids.len() > 1 { format!("{} ({})", g.name, g.pids.len()) } else { g.name.clone() },
                            [g.cpu, g.mem as f64, g.disk, -1.0, g.gpu],
                        ),
                        Some(pid) => {
                            let p = self.s.procs.iter().find(|p| p.pid == pid);
                            (p.map(|p| format!("{} ({})", p.image, p.pid)).unwrap_or_default(), p.map(|p| [p.cpu, p.mem as f64, p.disk, -1.0, p.gpu]).unwrap_or_default())
                        }
                    };
                    let hit = Hit::Row(lead, child);
                    let rect = r(name_x, y0, w - 16.0 * k, y0 + row_h);
                    unsafe {
                        if self.selected == Some((lead, child)) {
                            ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.sel);
                        } else if self.hover == Some(hit) {
                            ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.hover);
                        }
                    }
                    self.hits.push((rect.left, rect.top.max(top), rect.right, rect.bottom, hit));
                    let indent = if child.is_some() { 40.0 } else { 24.0 } * k;
                    if child.is_none() && g.pids.len() > 1 {
                        let open = self.expanded.contains(&lead);
                        let gl = if open { '\u{E70D}' } else { '\u{E76C}' };
                        draw(&ctx, &self.text, &self.f.glyph, &gl.to_string(), name_x, name_x + 24.0 * k, base + 2.0 * k, &b.dim, false);
                        self.hits.push((name_x, y0.max(top), name_x + 24.0 * k, y0 + row_h, Hit::Expand(lead)));
                    }
                    draw(&ctx, &self.text, &self.f.body, &name, name_x + indent, edges[0].0 - 8.0 * k, base, &b.text, false);
                    let norm = [vals[0] / ncpu, vals[1] / mem_total * 8.0, vals[2] / 10_485_760.0, 0.0, vals[4] / 100.0];
                    for i in 0..5 {
                        let (x0, x1) = edges[i];
                        heat(&b.heat, norm[i], self.light);
                        unsafe { ctx.FillRectangle(&r(x0, y0, x1, y0 + row_h), &b.heat) };
                        let s = match i {
                            0 => pct(vals[0]),
                            1 => mb(vals[1] as u64),
                            2 => rate(vals[2]),
                            3 => String::new(),
                            _ => pct(vals[4]),
                        };
                        draw(&ctx, &self.text, &self.f.right, &s, x0, x1 - 8.0 * k, base, &b.text, true);
                    }
                }
            }
        }
        unsafe { ctx.PopAxisAlignedClip() };
    }

    fn graph(&self, b: &B, h: &History, max: f64, rect: D2D_RECT_F, grid: bool) {
        let ctx = &self.surface.ctx;
        let k = self.k;
        unsafe {
            ctx.FillRectangle(&rect, &b.card);
            if grid {
                for i in 1..10 {
                    let x = rect.left + (rect.right - rect.left) * i as f32 / 10.0;
                    ctx.DrawLine(Vector2 { X: x, Y: rect.top }, Vector2 { X: x, Y: rect.bottom }, &b.line, 1.0, None);
                }
                for i in 1..4 {
                    let y = rect.top + (rect.bottom - rect.top) * i as f32 / 4.0;
                    ctx.DrawLine(Vector2 { X: rect.left, Y: y }, Vector2 { X: rect.right, Y: y }, &b.line, 1.0, None);
                }
            }
            ctx.DrawRectangle(&rect, &b.accent, 1.0 * k, None);
            let n = History::LEN as f32 - 1.0;
            let pts: Vec<Vector2> = h
                .0
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let x = rect.right - (h.0.len() - 1 - i) as f32 / n * (rect.right - rect.left);
                    let y = rect.bottom - ((v / max.max(1e-9)).clamp(0.0, 1.0) as f32) * (rect.bottom - rect.top);
                    Vector2 { X: x, Y: y }
                })
                .collect();
            if pts.len() < 2 {
                return;
            }
            let Ok(geo) = self.surface.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geo.Open() else { return };
            sink.BeginFigure(Vector2 { X: pts[0].X, Y: rect.bottom }, D2D1_FIGURE_BEGIN_FILLED);
            for p in &pts {
                sink.AddLine(*p);
            }
            sink.AddLine(Vector2 { X: pts[pts.len() - 1].X, Y: rect.bottom });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            ctx.FillGeometry(&geo, &b.accent_fill, None);
            for w in pts.windows(2) {
                ctx.DrawLine(w[0], w[1], &b.accent, 1.0 * k, None);
            }
        }
    }

    fn performance(&mut self, b: &B) {
        let k = self.k;
        let (w, h) = (self.size.0 as f32, self.size.1 as f32);
        let title = self.t("Performance", "Performances").to_string();
        self.header(b, &title);
        let t = &self.s.totals;
        let net_max = t.net.0.iter().fold(125_000.0f64, |a, &v| a.max(v)) * 1.2;
        let disk_max = t.disk.0.iter().fold(1_048_576.0f64, |a, &v| a.max(v)) * 1.2;
        let tiles: [(&str, String, &History, f64); 5] = [
            (self.t("CPU", "Processeur"), pct(t.cpu.last()), &t.cpu, 100.0),
            (self.t("Memory", "Mémoire"), format!("{}/{} ({:.0}%)", mb(t.mem_used), mb(t.mem_total), t.mem.last()), &t.mem, 100.0),
            (self.t("Disk (all I/O)", "Disque (toutes E/S)"), rate(t.disk.last()), &t.disk, disk_max),
            (self.t("Network", "Réseau"), format!("{} {} {} {}", self.t("S:", "E:"), mbps(t.net_out), self.t("R:", "R:"), mbps(t.net_in)), &t.net, net_max),
            ("GPU", pct(t.gpu.last()), &t.gpu, 100.0),
        ];
        let (lx0, lx1) = (60.0 * k, 280.0 * k);
        for (i, (name, val, hist, max)) in tiles.iter().enumerate() {
            let y0 = (60.0 + i as f32 * 68.0) * k;
            let rect = r(lx0, y0, lx1, y0 + 62.0 * k);
            let ctx = &self.surface.ctx;
            unsafe {
                if self.tile == i {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.sel);
                } else if self.hover == Some(Hit::Tile(i)) {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.hover);
                }
            }
            self.graph(b, hist, *max, r(lx0 + 8.0 * k, y0 + 10.0 * k, lx0 + 70.0 * k, y0 + 52.0 * k), false);
            draw(ctx, &self.text, &self.f.bold, name, lx0 + 80.0 * k, lx1, y0 + 26.0 * k, &b.text, false);
            draw(ctx, &self.text, &self.f.body, val, lx0 + 80.0 * k, lx1, y0 + 46.0 * k, &b.dim, true);
            self.hits.push((rect.left, rect.top, rect.right, rect.bottom, Hit::Tile(i)));
        }
        let (name, val, hist, max) = &tiles[self.tile.min(4)];
        let gx0 = lx1 + 24.0 * k;
        let ctx = &self.surface.ctx;
        draw(ctx, &self.text, &self.f.title, name, gx0, w - 16.0 * k, 84.0 * k, &b.text, false);
        let gr = r(gx0, 100.0 * k, w - 24.0 * k, (h - 130.0 * k).max(160.0 * k));
        self.graph(b, hist, *max, gr, true);
        draw(ctx, &self.text, &self.f.body, self.t("60 seconds", "60 secondes"), gx0, w, gr.bottom + 16.0 * k, &b.dim, false);
        let lines = [
            format!("{} {val}", self.t("Now:", "Actuel :")),
            format!("{} {}    {} {}", self.t("Processes:", "Processus :"), t.procs, self.t("Threads:", "Threads :"), t.threads),
            format!("{} {:.1} ms", self.t("Sample cost:", "Coût d'un relevé :"), self.s.cost_ms),
        ];
        for (i, l) in lines.iter().enumerate() {
            draw(ctx, &self.text, &self.f.body, l, gx0, w, gr.bottom + (40.0 + i as f32 * 20.0) * k, &b.text, true);
        }
    }

    fn startup_page(&mut self, b: &B) {
        let k = self.k;
        let w = self.size.0 as f32;
        let title = self.t("Startup apps", "Applications de démarrage").to_string();
        let x = self.header(b, &title);
        let sel = self.startup_sel.and_then(|i| self.startup.get(i)).map(|e| e.enabled);
        let label = if sel == Some(false) { self.t("Enable", "Activer") } else { self.t("Disable", "Désactiver") }.to_string();
        self.button(b, x, if sel == Some(false) { '\u{E73E}' } else { '\u{E711}' }, &label, Hit::Toggle, sel.is_some());
        let ctx = self.surface.ctx.clone();
        let (c0, c1, c2) = (64.0 * k, 300.0 * k, 400.0 * k);
        let hy = 84.0 * k;
        draw(&ctx, &self.text, &self.f.body, self.t("Name", "Nom"), c0 + 8.0 * k, c1, hy, &b.dim, false);
        draw(&ctx, &self.text, &self.f.body, self.t("Status", "Statut"), c1, c2, hy, &b.dim, false);
        draw(&ctx, &self.text, &self.f.body, self.t("Source and command", "Source et commande"), c2, w, hy, &b.dim, false);
        unsafe { ctx.FillRectangle(&r(c0, hy + 10.0 * k, w - 16.0 * k, hy + 11.0 * k), &b.line) };
        if self.startup.is_empty() {
            draw(&ctx, &self.text, &self.f.body, self.t("No startup apps", "Aucune application de démarrage"), c0 + 8.0 * k, w, hy + 36.0 * k, &b.dim, false);
        }
        for (i, e) in self.startup.iter().enumerate() {
            let y0 = hy + 14.0 * k + i as f32 * 32.0 * k;
            let rect = r(c0, y0, w - 16.0 * k, y0 + 30.0 * k);
            unsafe {
                if self.startup_sel == Some(i) {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.sel);
                } else if self.hover == Some(Hit::Startup(i)) {
                    ctx.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: 4.0 * k, radiusY: 4.0 * k }, &b.hover);
                }
            }
            let base = y0 + 20.0 * k;
            draw(&ctx, &self.text, &self.f.body, &e.name, c0 + 8.0 * k, c1 - 8.0 * k, base, &b.text, false);
            let st = if e.enabled { self.t("Enabled", "Activé") } else { self.t("Disabled", "Désactivé") };
            draw(&ctx, &self.text, &self.f.body, st, c1, c2, base, &b.text, false);
            draw(&ctx, &self.text, &self.f.body, &format!("{}: {}", e.source, e.command), c2, w - 16.0 * k, base, &b.dim, false);
            self.hits.push((rect.left, rect.top, rect.right, rect.bottom, Hit::Startup(i)));
        }
    }
}
