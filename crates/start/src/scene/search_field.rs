//! The drawn search field. DirectWrite measures the same layout for painting and mouse hits.

use super::{GLYPH_SEARCH, L, SECONDARY, Start, TERTIARY, WHITE, r, rgba};
use crate::{edit::byte_at_utf16, layout::Rect};
use render::Color;
use windows::Win32::Graphics::Direct2D::{D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE};
use windows::Win32::Graphics::DirectWrite::{DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, IDWriteTextLayout};
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows_core::BOOL;
use windows_numerics::Vector2;

const LEFT: f32 = L::SEARCH.x + 44.0;
const RIGHT: f32 = L::SEARCH.x + L::SEARCH.w - 16.0;
const TOP: f32 = L::SEARCH.y + 7.0;
const HEIGHT: f32 = 18.0;

pub(super) fn accent() -> Color {
    let mut argb = 0xff00_78d4;
    let mut opaque = BOOL::default();
    // SAFETY: DWM fills two local values.
    let _ = unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) };
    rgba((argb >> 16) as u8, (argb >> 8) as u8, argb as u8, 0.72)
}

impl Start {
    fn search_layout(&self) -> Option<IDWriteTextLayout> {
        let text: Vec<u16> = self.query.encode_utf16().collect();
        // SAFETY: the text and format live through creation; wrapping is disabled.
        unsafe { self.text.factory.CreateTextLayout(&text, &self.search_text, RIGHT - LEFT, L::SEARCH.h) }.ok()
    }

    /// Use the nearest insertion point for a click. For a double click, use the hit cluster.
    pub fn search_hit(&self, x: f32, word: bool) -> usize {
        let Some(layout) = self.search_layout() else { return 0 };
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        let mut trailing = BOOL::default();
        let mut inside = BOOL::default();
        // SAFETY: local out parameters; y sits in the single line.
        let result = unsafe { layout.HitTestPoint(x - LEFT + self.search_offset, 5.0, &mut trailing, &mut inside, &mut metrics) };
        if result.is_err() {
            return self.query.len();
        }
        let offset = if trailing.as_bool() && (!word || !inside.as_bool()) { metrics.length } else { 0 };
        byte_at_utf16(&self.query, metrics.textPosition + offset)
    }

    pub fn refresh_accent(&mut self) {
        self.selection_color = accent();
        self.dirty = true;
    }

    pub(super) fn draw_search(&mut self) {
        let b = L::SEARCH;
        self.fill(&b, L::SEARCH_RADIUS, rgba(0, 0, 0, 0.28));
        self.stroke(&b, L::SEARCH_RADIUS, rgba(255, 255, 255, 0.09));
        self.txt(&self.glyph, GLYPH_SEARCH, b.x + 9.0, b.x + 37.0, b.y + 23.0, SECONDARY);
        let base = b.y + 21.0;
        if self.query.is_empty() {
            self.search_offset = 0.0;
            self.txt(&self.body, self.s.search, LEFT, RIGHT, base, TERTIARY);
            if self.search_focus {
                self.fill(&Rect::new(LEFT, TOP, 1.0, HEIGHT), 0.0, WHITE);
            }
            return;
        }
        let Some(layout) = self.search_layout() else { return };
        let utf16_at = |at| self.query[..at].encode_utf16().count() as u32;
        let mut caret_x = 0.0;
        let mut caret_y = 0.0;
        let mut hit = DWRITE_HIT_TEST_METRICS::default();
        let mut lines = [DWRITE_LINE_METRICS::default(); 1];
        let mut count = 0;
        // SAFETY: local out parameters and a position in the current text layout.
        unsafe {
            let _ = layout.HitTestTextPosition(utf16_at(self.edit.caret), false, &mut caret_x, &mut caret_y, &mut hit);
            let _ = layout.GetLineMetrics(Some(&mut lines), &mut count);
        }
        let mut metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
        // SAFETY: reads layout dimensions into a local struct.
        let _ = unsafe { layout.GetMetrics(&mut metrics) };
        let width = RIGHT - LEFT - 1.0;
        if caret_x < self.search_offset {
            self.search_offset = caret_x;
        } else if caret_x > self.search_offset + width {
            self.search_offset = caret_x - width;
        }
        self.search_offset = self.search_offset.clamp(0.0, (metrics.widthIncludingTrailingWhitespace - width).max(0.0));

        let left = LEFT - self.search_offset;
        let selection = self.edit.selection();
        let ctx = &self.surface.ctx;
        // SAFETY: within BeginDraw; the clip applies to text, selection and caret only.
        unsafe { ctx.PushAxisAlignedClip(&r(&Rect::new(LEFT, b.y, RIGHT - LEFT, b.h)), D2D1_ANTIALIAS_MODE_ALIASED) };
        if self.search_focus && !selection.is_empty() {
            let start = utf16_at(selection.start);
            let length = utf16_at(selection.end) - start;
            let mut count = 0;
            // SAFETY: first query gets the size; the second writes those rectangles.
            unsafe {
                let _ = layout.HitTestTextRange(start, length, left, 0.0, None, &mut count);
                let mut boxes = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
                if layout.HitTestTextRange(start, length, left, 0.0, Some(&mut boxes), &mut count).is_ok() {
                    for part in boxes {
                        self.fill(&Rect::new(part.left, TOP, part.width, HEIGHT), 0.0, self.selection_color);
                    }
                }
            }
        }
        // SAFETY: lifetime of layout and brush cover this draw.
        unsafe {
            ctx.DrawTextLayout(
                Vector2 { X: left, Y: base - lines[0].baseline }, &layout, self.color(WHITE), D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
        if self.search_focus && selection.is_empty() {
            self.fill(&Rect::new((left + caret_x).floor(), TOP, 1.0, HEIGHT), 0.0, WHITE);
        }
        // SAFETY: balances the clip above.
        unsafe { ctx.PopAxisAlignedClip() };
    }
}
