//! Window chrome: tab strip with caption buttons, navigation bar with breadcrumb and
//! search box, command bar. The backdrop shows through everything not drawn here.

use windows::Win32::Graphics::Direct2D::Common::{D2D_SIZE_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
use windows::Win32::Graphics::Direct2D::{D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE};
use windows_core::Result;
use windows_numerics::Vector2;

use crate::groups;
use crate::layout as L;
use crate::paint::{Painter, rect};
use crate::scene::Scene;

pub fn draw(p: &Painter, sc: &Scene) -> Result<()> {
    tab_strip(p, sc)?;
    nav_bar(p, sc)?;
    command_bar(p, sc)
}

/// Tab with rounded top corners, merging into the navigation bar below it.
fn tab_shape(p: &Painter, fill: render::Color, stroke: render::Color) -> Result<()> {
    let (x, y, w, h) = L::TAB;
    let r = L::TAB_RADIUS;
    unsafe {
        let geo = p.s.factory.CreatePathGeometry()?;
        let sink = geo.Open()?;
        sink.BeginFigure(Vector2 { X: x, Y: y + h }, D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(Vector2 { X: x, Y: y + r });
        let arc = |px: f32, py: f32| D2D1_ARC_SEGMENT {
            point: Vector2 { X: px, Y: py },
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        };
        sink.AddArc(&arc(x + r, y));
        sink.AddLine(Vector2 { X: x + w - r, Y: y });
        sink.AddArc(&arc(x + w, y + r));
        sink.AddLine(Vector2 { X: x + w, Y: y + h });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        p.s.ctx.FillGeometry(&geo, &p.s.brush(fill)?, None);
        // Stroke on the outer edges only: clip away the bottom edge.
        p.s.ctx.PushAxisAlignedClip(&rect(x - 1.0, y - 1.0, w + 2.0, h - 0.5), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED);
        p.s.ctx.DrawGeometry(&geo, &p.s.brush(stroke)?, 1.0, None);
        p.s.ctx.PopAxisAlignedClip();
    }
    Ok(())
}

fn tab_strip(p: &Painter, sc: &Scene) -> Result<()> {
    tab_shape(p, L::LAYER, L::TAB_STROKE)?;
    if let Some(icon) = &sc.tab_icon {
        p.bitmap(icon, L::TAB_ICON.0, L::TAB_ICON.1, 16.0);
    }
    let b = L::baseline(L::TAB_CY, 12.0);
    p.text(&p.f.ui12, &sc.listing.title, L::TAB_TEXT_X, L::TAB_CLOSE_CX - 14.0, b, L::TEXT)?;
    p.glyph(&p.f.glyph10, '\u{E8BB}', L::TAB_CLOSE_CX, L::TAB_CY, 10.0, L::TEXT)?;
    p.glyph(&p.f.glyph12, '\u{E710}', L::TAB_PLUS_CX, L::TAB_CY, 12.0, L::TEXT)?;
    for (g, off) in ['\u{E921}', '\u{E922}', '\u{E8BB}'].into_iter().zip(L::CAPTION_CX) {
        p.glyph(&p.f.glyph10, g, sc.w - off, L::CAPTION_CY, 10.0, L::CAPTION_GLYPH)?;
    }
    Ok(())
}

fn nav_bar(p: &Painter, sc: &Scene) -> Result<()> {
    let w = sc.w;
    p.fill(rect(1.0, L::TABSTRIP_H + 8.0, w - 2.0, L::NAVBAR_BOTTOM - L::TABSTRIP_H - 8.0), L::LAYER)?;
    // The tab's own fill covers 8..48 at its x; the bar fill starts under it (y 48).
    p.fill(rect(1.0, L::TABSTRIP_H, L::TAB.0 - 1.0, 8.0), L::LAYER)?;
    p.fill(rect(L::TAB.0 + L::TAB.2, L::TABSTRIP_H, w - 1.0 - L::TAB.0 - L::TAB.2, 8.0), L::LAYER)?;
    let glyphs = [('\u{E72B}', false), ('\u{E72A}', false), ('\u{E74A}', true), ('\u{E72C}', true)];
    for ((g, on), cx) in glyphs.into_iter().zip(L::NAV_BTN_CX) {
        p.glyph(&p.f.glyph14, g, cx, L::NAV_CY, 14.0, if on { L::TEXT } else { L::TEXT_DISABLED })?;
    }
    // Address box with the breadcrumb.
    let (ax, ar) = L::ADDR;
    p.round(rect(ax, L::BOX_TOP, w - ar - ax, L::BOX_H), L::BOX_RADIUS, L::CONTROL_FILL, None)?;
    p.glyph(&p.f.glyph16, '\u{E7F4}', L::ADDR_PC_CX, L::NAV_CY, 16.0, L::TEXT)?;
    let b = L::baseline(L::NAV_CY, 14.0);
    let mut chevron = L::CRUMB_FIRST_CHEVRON_CX;
    for c in &sc.listing.crumbs {
        p.glyph(&p.f.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, L::TEXT)?;
        let x = chevron + L::CRUMB_GAP + 1.0;
        p.text(&p.f.ui14, c, x, w - ar, b, L::TEXT)?;
        chevron = x + p.measure(&p.f.ui14, c) + L::CRUMB_GAP;
    }
    p.glyph(&p.f.glyph10, '\u{E76C}', chevron, L::NAV_CY, 10.0, L::TEXT)?;
    // Search box.
    let (sl, sr) = L::SEARCH;
    p.round(rect(w - sl, L::BOX_TOP, sl - sr, L::BOX_H), L::BOX_RADIUS, L::CONTROL_FILL, None)?;
    let hint = if groups::is_french() { format!("Rechercher dans : {}", sc.listing.title) } else { format!("Search {}", sc.listing.title) };
    p.text(&p.f.ui14, &hint, w - sl + L::SEARCH_TEXT_PAD, w - L::SEARCH_GLYPH_FROM_RIGHT - 12.0, b, L::TEXT_DIM)?;
    p.glyph(&p.f.glyph12, '\u{E721}', w - L::SEARCH_GLYPH_FROM_RIGHT, L::NAV_CY, 12.0, L::TEXT)?;
    Ok(())
}

fn command_bar(p: &Painter, sc: &Scene) -> Result<()> {
    let w = sc.w;
    p.fill(rect(1.0, L::NAVBAR_BOTTOM, w - 2.0, L::CMDBAR_BOTTOM - L::NAVBAR_BOTTOM), L::LAYER)?;
    p.fill(rect(1.0, L::CMDBAR_BOTTOM, w - 2.0, 1.0), L::CMDBAR_LINE)?;
    let cy = L::CMD_CY;
    let b = L::baseline(cy, 12.0);
    let labels = if groups::is_french() { ["Nouveau", "Trier", "Afficher"] } else { ["New", "Sort", "View"] };
    for ((g, gx, tx, chev), label) in L::CMD_LABELLED.into_iter().zip(labels) {
        p.glyph(&p.f.glyph16, g, gx, cy, 16.0, L::TEXT)?;
        p.text(&p.f.ui12, label, tx, tx + 80.0, b, L::TEXT)?;
        p.glyph(&p.f.glyph10, '\u{E70D}', chev, cy, 8.0, L::TEXT_DIM)?;
    }
    for x in L::CMD_SEPS {
        p.fill(rect(x, cy - 16.0, 1.0, 32.0), L::CMD_SEP)?;
    }
    for (g, cx, on) in L::CMD_ICONS {
        let c = if !on { L::TEXT_DISABLED } else if g == '\u{E74D}' { L::TEXT } else { L::ACCENT_ICON };
        p.glyph(&p.f.glyph16, g, cx, cy, 16.0, c)?;
    }
    p.glyph(&p.f.glyph16, '\u{E712}', L::CMD_MORE_CX, cy, 16.0, L::TEXT)?;
    let (gx, tx) = L::CMD_DETAILS;
    p.glyph(&p.f.glyph16, '\u{E8A0}', w - gx, cy, 16.0, L::ACCENT_ICON)?;
    p.text(&p.f.ui12, if groups::is_french() { "D\u{e9}tails" } else { "Details" }, w - tx, w, b, L::TEXT)?;
    Ok(())
}
