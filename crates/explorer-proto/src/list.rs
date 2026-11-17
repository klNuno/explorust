//! Details view: column header, date groups, rows with real icons, selection and hover,
//! scroll bars. Rows below the visible area are laid out but not drawn.

use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE;
use windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE;
use windows_core::Result;

use crate::groups;
use crate::layout as L;
use crate::paint::{Painter, rect};
use crate::scene::Scene;
use crate::shell::Listing;

pub enum Row {
    Group { label: &'static str, y: f32 },
    Item { entry: usize, y: f32 },
}

pub fn rows(l: &Listing) -> Vec<Row> {
    let now = groups::now();
    let mut out = Vec::new();
    let mut y = L::CONTENT_TOP;
    let mut last = None;
    for (i, e) in l.entries.iter().enumerate() {
        let b = groups::bucket(e.modified, now);
        if last != Some(b) {
            out.push(Row::Group { label: groups::label(b), y });
            y += L::GROUP_H;
            last = Some(b);
        }
        out.push(Row::Item { entry: i, y });
        y += L::ROW_H;
    }
    out
}

fn list_bottom(h: f32) -> f32 {
    h - L::STATUS_H - L::HSCROLL_H
}

pub fn visible_entries(rows: &[Row], h: f32) -> Vec<usize> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Item { entry, y } if *y < list_bottom(h) => Some(*entry),
            _ => None,
        })
        .collect()
}

pub fn entry_at(rows: &[Row], x: f32, y: f32, w: f32, h: f32) -> Option<usize> {
    if x < L::LIST_X || x >= w - L::VSCROLL_W || y < L::HEADER_BOTTOM || y >= list_bottom(h) {
        return None;
    }
    rows.iter().find_map(|r| match r {
        Row::Item { entry, y: ry } if y >= *ry && y < ry + L::ROW_H => Some(*entry),
        _ => None,
    })
}

pub fn draw(p: &Painter, sc: &Scene) -> Result<()> {
    let (w, h) = (sc.w, sc.h);
    let right = w - L::VSCROLL_W;
    let bottom = list_bottom(h);
    let ctx = &p.s.ctx;
    p.fill(rect(L::LIST_X, L::CMDBAR_BOTTOM + 1.0, w - L::LIST_X - 1.0, h - L::CMDBAR_BOTTOM - 1.0 - L::STATUS_H), L::CONTENT_BG)?;
    unsafe {
        ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
        ctx.PushAxisAlignedClip(&rect(L::LIST_X, L::CMDBAR_BOTTOM + 1.0, right - L::LIST_X, bottom - L::CMDBAR_BOTTOM - 1.0), D2D1_ANTIALIAS_MODE_ALIASED);
    }
    // Column header.
    let hb = L::baseline(L::HEADER_CY, 12.0);
    p.text(&p.f.list, if groups::is_french() { "Nom" } else { "Name" }, L::HEADER_NAME_X, L::COLUMNS[0], hb, L::TEXT_DIM)?;
    let heads = if groups::is_french() { ["Modifi\u{e9} le", "Type", "Taille"] } else { ["Date modified", "Type", "Size"] };
    for (i, name) in heads.iter().enumerate() {
        let x0 = L::COLUMNS[i];
        p.fill(rect(x0, L::CMDBAR_BOTTOM + 1.0, 1.0, L::HEADER_BOTTOM - L::CMDBAR_BOTTOM - 1.0), L::HEADER_SEP)?;
        p.text(&p.f.list, name, x0 + L::COL_PAD, L::COLUMNS[i + 1], hb, L::TEXT_DIM)?;
    }
    // Sort indicator above "Date modified".
    p.glyph(&p.f.glyph10, '\u{E70D}', L::COLUMNS[0] + 72.0, L::CMDBAR_BOTTOM + 3.0, 8.0, L::TEXT_DIM)?;
    unsafe { ctx.PushAxisAlignedClip(&rect(L::LIST_X, L::HEADER_BOTTOM, right - L::LIST_X, bottom - L::HEADER_BOTTOM), D2D1_ANTIALIAS_MODE_ALIASED) };
    for r in &sc.rows {
        match r {
            Row::Group { label, y } if *y < bottom => {
                let cy = y + L::GROUP_H / 2.0;
                p.glyph(&p.f.glyph10, '\u{E70D}', L::GROUP_CHEVRON_CX, cy, 10.0, L::GROUP_CHEVRON)?;
                p.text(&p.f.group, label, L::GROUP_TEXT_X, right, L::baseline(cy, 15.0), L::TEXT)?;
            }
            Row::Item { entry, y } if *y < bottom => {
                let sel = sc.selected == Some(*entry);
                let hov = sc.hovered == Some(*entry);
                if sel || hov {
                    let r = rect(L::SEL_X0 + 0.5, y + 2.5, right - L::SEL_X0 - 1.0, L::ROW_H - 5.0);
                    p.round(r, 0.0, if sel { L::SEL_FILL } else { L::HOVER_FILL }, sel.then_some(L::SEL_BORDER))?;
                }
                if let Some(icon) = &sc.icons[*entry] {
                    p.bitmap(icon, L::ROW_ICON_X, y + 6.0, 16.0);
                }
                let e = &sc.listing.entries[*entry];
                let b = L::baseline(y + L::ROW_H / 2.0, 12.0);
                p.text(&p.f.list, &e.name, L::ROW_TEXT_X, L::COLUMNS[0] - 4.0, b, L::TEXT)?;
                p.text(&p.f.list, &e.date, L::COLUMNS[0] + L::COL_PAD, L::COLUMNS[1] - 4.0, b, L::TEXT_DIM)?;
                p.text(&p.f.list, &e.kind, L::COLUMNS[1] + L::COL_PAD, L::COLUMNS[2] - 4.0, b, L::TEXT_DIM)?;
                p.text(&p.f.list_right, &e.size, L::COLUMNS[2] + L::COL_PAD, L::COLUMNS[3] - L::COL_PAD, b, L::TEXT_DIM)?;
            }
            _ => {}
        }
    }
    unsafe {
        ctx.PopAxisAlignedClip();
        ctx.PopAxisAlignedClip();
    }
    // Scroll bars: thin-line thumbs as the inactive window shows them.
    p.fill(rect(right, L::CMDBAR_BOTTOM + 1.0, L::VSCROLL_W, bottom - L::CMDBAR_BOTTOM - 1.0), L::SCROLL_BG)?;
    p.fill(rect(L::LIST_X, bottom, w - L::LIST_X - 1.0, L::HSCROLL_H), L::SCROLL_BG)?;
    let content_h = sc.rows.last().map(|r| match r { Row::Group { y, .. } | Row::Item { y, .. } => y + L::ROW_H }).unwrap_or(0.0) - L::HEADER_BOTTOM;
    let track = bottom - L::HEADER_BOTTOM - 4.0;
    let thumb = (track * (bottom - L::HEADER_BOTTOM) / content_h.max(1.0)).clamp(20.0, track);
    p.fill(rect(right + 8.0, L::HEADER_BOTTOM + 2.0, 2.0, thumb), L::SCROLL_THUMB)?;
    let view_w = right - L::LIST_X;
    let total_w = L::COLUMNS[3] - L::LIST_X;
    let hthumb = ((view_w - 4.0) * view_w / total_w).min(view_w - 4.0);
    p.fill(rect(L::LIST_X + 2.0, bottom + 8.0, hthumb, 2.0), L::SCROLL_THUMB)?;
    unsafe { ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
    Ok(())
}
