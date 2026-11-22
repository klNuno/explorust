#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewLayer {
    Opaque,
    ColorKey,
    Transparent,
}

pub fn view_layer(raised: bool, icons_visible: bool) -> ViewLayer {
    match (raised, icons_visible) {
        (false, _) => ViewLayer::Opaque,
        (true, true) => ViewLayer::ColorKey,
        (true, false) => ViewLayer::Transparent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raised_defview_with_hidden_icons_is_transparent() {
        assert_eq!(view_layer(true, false), ViewLayer::Transparent);
    }

    #[test]
    fn raised_defview_with_icons_keeps_them_visible() {
        assert_eq!(view_layer(true, true), ViewLayer::ColorKey);
    }

    #[test]
    fn unraised_defview_keeps_the_static_wallpaper() {
        assert_eq!(view_layer(false, true), ViewLayer::Opaque);
        assert_eq!(view_layer(false, false), ViewLayer::Opaque);
    }
}
