//! Layout of the hover thumbnail flyout, in DIPs: one card per window of a group, side by side,
//! each with a title row (app icon, title, close button) over the live thumbnail. Sizes follow
//! Windows 11's flyout at 100%.

/// Largest thumbnail box.
pub const THUMB_MAX_W: f32 = 200.0;
pub const THUMB_MAX_H: f32 = 120.0;
/// Narrowest card, for tall windows.
pub const CARD_MIN_W: f32 = 136.0;
pub const PAD: f32 = 8.0;
pub const TITLE_H: f32 = 32.0;
pub const CLOSE_W: f32 = 32.0;
pub const GAP: f32 = 4.0;
/// Gap between the flyout and the bar.
pub const ABOVE_BAR: f32 = 12.0;
pub const RADIUS: f32 = 4.0;
pub const ICON: f32 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Card {
    pub card: R,
    pub title: R,
    pub close: R,
    pub thumb: R,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    pub cards: Vec<Card>,
}

/// The thumbnail size of a `w` x `h` source inside the max box, aspect kept.
pub fn fit(w: f32, h: f32) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (THUMB_MAX_W, THUMB_MAX_H);
    }
    let k = (THUMB_MAX_W / w).min(THUMB_MAX_H / h);
    ((w * k).round(), (h * k).round())
}

/// Cards for sources of the given sizes (px or DIPs, only the ratio counts).
pub fn layout(sources: &[(f32, f32)]) -> Layout {
    let mut cards = Vec::with_capacity(sources.len());
    let mut x = GAP;
    for &(sw, sh) in sources {
        let (tw, th) = fit(sw, sh);
        let cw = (tw + 2.0 * PAD).max(CARD_MIN_W);
        let card = R { x, y: GAP, w: cw, h: TITLE_H + th + PAD };
        let title = R { x, y: GAP, w: cw, h: TITLE_H };
        let close = R { x: x + cw - CLOSE_W - 4.0, y: GAP + 4.0, w: CLOSE_W, h: TITLE_H - 8.0 };
        let thumb = R { x: x + (cw - tw) / 2.0, y: GAP + TITLE_H, w: tw, h: th };
        cards.push(Card { card, title, close, thumb });
        x += cw + GAP;
    }
    let h = cards.iter().map(|c| c.card.h).fold(0.0, f32::max) + 2.0 * GAP;
    // Cards of one flyout share the tallest height.
    for c in &mut cards {
        c.card.h = h - 2.0 * GAP;
    }
    Layout { w: x, h, cards }
}

/// What a point hits: (card, on its close button).
pub fn hit(l: &Layout, x: f32, y: f32) -> Option<(usize, bool)> {
    let i = l.cards.iter().position(|c| c.card.contains(x, y))?;
    Some((i, l.cards[i].close.contains(x, y)))
}

/// Left edge of the flyout centered on `anchor`, kept inside `min..max`.
pub fn place_x(anchor: f32, w: f32, min: f32, max: f32) -> f32 {
    (anchor - w / 2.0).clamp(min, (max - w).max(min))
}

/// What the flyout does when the pointer is over `slot` (None: elsewhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverStep {
    /// Nothing changes (the flyout or the delay already matches).
    Keep,
    /// The open flyout stays: cancel its close grace.
    Stay,
    /// Another button's flyout is open: show this one at once.
    Switch(usize),
    /// Start the close grace.
    CloseSoon,
    /// Start the hover delay for this button.
    Arm(usize),
    /// Cancel the hover delay.
    Disarm,
    /// Close the flyout now and cancel any delay.
    Close,
}

/// `open`: the button whose flyout shows; `want`: the button whose delay runs; `menu_up`: a
/// menu of the bar is open, which the flyout must never cover.
pub fn hover_step(open: Option<usize>, want: Option<usize>, slot: Option<usize>, menu_up: bool) -> HoverStep {
    match (open, slot) {
        (Some(_), _) if menu_up => HoverStep::Close,
        (None, _) if menu_up => HoverStep::Disarm,
        (Some(o), Some(i)) if o == i => HoverStep::Stay,
        (Some(_), Some(i)) => HoverStep::Switch(i),
        (Some(_), None) => HoverStep::CloseSoon,
        (None, Some(i)) if want == Some(i) => HoverStep::Keep,
        (None, Some(i)) => HoverStep::Arm(i),
        (None, None) => HoverStep::Disarm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_without_menu() {
        assert_eq!(hover_step(None, None, Some(3), false), HoverStep::Arm(3));
        assert_eq!(hover_step(None, Some(3), Some(3), false), HoverStep::Keep);
        assert_eq!(hover_step(Some(3), None, Some(3), false), HoverStep::Stay);
        assert_eq!(hover_step(Some(3), None, Some(4), false), HoverStep::Switch(4));
        assert_eq!(hover_step(Some(3), None, None, false), HoverStep::CloseSoon);
        assert_eq!(hover_step(None, Some(3), None, false), HoverStep::Disarm);
    }

    /// A right click opens the jump list over the button the pointer rests on: the flyout must
    /// not come back on top of the menu.
    #[test]
    fn menu_keeps_flyout_away() {
        assert_eq!(hover_step(None, None, Some(8), true), HoverStep::Disarm);
        assert_eq!(hover_step(None, Some(8), Some(8), true), HoverStep::Disarm);
        assert_eq!(hover_step(Some(8), None, Some(8), true), HoverStep::Close);
        assert_eq!(hover_step(Some(8), None, Some(7), true), HoverStep::Close);
    }

    #[test]
    fn fit_keeps_ratio() {
        assert_eq!(fit(1280.0, 800.0), (192.0, 120.0));
        assert_eq!(fit(1600.0, 400.0), (200.0, 50.0));
        assert_eq!(fit(400.0, 800.0), (60.0, 120.0));
        assert_eq!(fit(0.0, 10.0), (200.0, 120.0));
    }

    #[test]
    fn cards_side_by_side() {
        let l = layout(&[(1280.0, 800.0), (400.0, 800.0)]);
        assert_eq!(l.cards.len(), 2);
        assert_eq!(l.cards[0].card.x, GAP);
        assert_eq!(l.cards[0].card.w, 208.0);
        assert_eq!(l.cards[1].card.x, GAP + 208.0 + GAP);
        assert_eq!(l.cards[1].card.w, CARD_MIN_W);
        assert_eq!(l.w, GAP + 208.0 + GAP + CARD_MIN_W + GAP);
        assert_eq!(l.h, TITLE_H + 120.0 + PAD + 2.0 * GAP);
        assert_eq!(l.cards[1].thumb.w, 60.0);
        assert_eq!(l.cards[1].thumb.x, l.cards[1].card.x + (CARD_MIN_W - 60.0) / 2.0);
    }

    #[test]
    fn hits_card_and_close() {
        let l = layout(&[(1280.0, 800.0)]);
        let c = l.cards[0];
        assert_eq!(hit(&l, c.thumb.x + 5.0, c.thumb.y + 5.0), Some((0, false)));
        assert_eq!(hit(&l, c.close.x + 2.0, c.close.y + 2.0), Some((0, true)));
        assert_eq!(hit(&l, 0.5, 0.5), None);
    }

    #[test]
    fn placement_clamps() {
        assert_eq!(place_x(640.0, 200.0, 0.0, 1280.0), 540.0);
        assert_eq!(place_x(20.0, 200.0, 12.0, 1268.0), 12.0);
        assert_eq!(place_x(1270.0, 200.0, 12.0, 1268.0), 1068.0);
    }
}
