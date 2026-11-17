//! Navigation pane (Quick access pins, This PC, Network) and the status bar.

use windows::Win32::Graphics::Direct2D::{D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE};
use windows_core::Result;

use crate::groups;
use crate::layout as L;
use crate::paint::{Painter, rect};
use crate::scene::Scene;

pub fn draw(p: &Painter, sc: &Scene) -> Result<()> {
    let (w, h) = (sc.w, sc.h);
    let top = L::CMDBAR_BOTTOM + 1.0;
    let status_top = h - L::STATUS_H;
    p.fill(rect(1.0, top, L::NAV_W - 1.0, status_top - top), L::CONTENT_BG)?;
    for (dx, c) in L::SPLITTER {
        p.fill(rect(L::NAV_W + dx, top, 1.0, status_top - top), c)?;
    }
    unsafe { p.s.ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE) };
    let mut slot = 0.0;
    let mut pins_done = false;
    for (i, n) in sc.nav.iter().enumerate() {
        if !n.pinned && !pins_done {
            // Separator between the pins and This PC takes one slot.
            let y = L::NAV_FIRST_CY + slot * L::NAV_PITCH - 7.0;
            p.fill(rect(4.0, y.round(), L::NAV_W - 8.0, 1.0), L::NAV_SEPARATOR)?;
            slot += 1.0;
            pins_done = true;
        }
        let cy = L::NAV_FIRST_CY + slot * L::NAV_PITCH;
        // The folder shown lives under This PC, so its row carries the inactive selection.
        if i == sc.nav.iter().position(|n| !n.pinned).unwrap_or(usize::MAX) {
            p.round(rect(3.5, cy - 15.0 + 0.5, L::NAV_W - 4.0, 31.0), 0.0, L::NAV_SEL_FILL, Some(L::NAV_SEL_BORDER))?;
        }
        if n.expandable {
            p.glyph(&p.f.glyph10, '\u{E76C}', L::NAV_CHEVRON_CX, cy, 10.0, L::TEXT)?;
        }
        if let Some(icon) = &sc.nav_icons[i] {
            p.bitmap(icon, L::NAV_ICON_X, cy - 8.0, 16.0);
        }
        p.text(&p.f.list, &n.name, L::NAV_TEXT_X, L::NAV_PIN_CX - 10.0, L::baseline(cy, 12.0), L::TEXT)?;
        if n.pinned {
            p.glyph(&p.f.glyph12, '\u{E840}', L::NAV_PIN_CX, cy, 12.0, L::CAPTION_GLYPH)?;
        }
        slot += 1.0;
    }
    // Status bar.
    p.fill(rect(1.0, status_top, w - 2.0, L::STATUS_H - 1.0), L::STATUS_BG)?;
    let n = sc.listing.entries.len();
    let fr = groups::is_french();
    let count = if fr { format!("{n} \u{e9}l\u{e9}ment(s)") } else { format!("{n} items") };
    let b = L::baseline(status_top + 11.5, 12.0);
    p.text(&p.f.list, &count, L::STATUS_TEXT_X, 400.0, b, L::TEXT)?;
    p.fill(rect(L::STATUS_SEP_X[0], status_top + 5.0, 1.0, 14.0), L::TEXT_DIM)?;
    if sc.selected.is_some() {
        let s = if fr { "1 \u{e9}l\u{e9}ment s\u{e9}lectionn\u{e9}" } else { "1 item selected" };
        p.text(&p.f.list, s, L::STATUS_SEP_X[0] + 12.0, 400.0, b, L::TEXT)?;
        p.fill(rect(L::STATUS_SEP_X[1], status_top + 5.0, 1.0, 14.0), L::TEXT_DIM)?;
    }
    unsafe { p.s.ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };
    let cy = status_top + 11.5;
    p.round(rect(w - L::STATUS_VIEW_CX[0] - 11.5, cy - 10.5, 22.0, 21.0), 2.0, L::SEL_FILL, None)?;
    p.glyph(&p.f.glyph12, '\u{E8FD}', w - L::STATUS_VIEW_CX[0], cy, 12.0, L::TEXT)?;
    p.glyph(&p.f.glyph12, '\u{E8A9}', w - L::STATUS_VIEW_CX[1], cy, 12.0, L::TEXT)?;
    Ok(())
}
