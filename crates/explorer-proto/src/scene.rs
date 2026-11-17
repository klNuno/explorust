//! The Explorer window content: tab strip, navigation bar, command bar (chrome.rs),
//! navigation pane and status bar (pane.rs), details view (list.rs).

use render::Surface;
use render::text::Text;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows_core::Result;

use crate::paint::{Fonts, Painter};
use crate::shell::{Listing, NavItem};
use crate::{chrome, list, pane};

pub struct Scene {
    pub surface: Surface,
    pub text: Text,
    pub fonts: Fonts,
    pub w: f32,
    pub h: f32,
    pub listing: Listing,
    pub rows: Vec<list::Row>,
    pub icons: Vec<Option<ID2D1Bitmap1>>, // per entry, loaded for visible rows
    pub nav: Vec<NavItem>,
    pub nav_icons: Vec<Option<ID2D1Bitmap1>>,
    pub tab_icon: Option<ID2D1Bitmap1>,
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
}

pub fn bmp(s: &Surface, path: &str, size: i32) -> Option<ID2D1Bitmap1> {
    let img = shelldata::icons::icon_for_path(path, size)?;
    s.bitmap(img.width, img.height, &img.to_premultiplied_bgra()).ok()
}

impl Scene {
    pub fn new(hwnd: HWND, w: u32, h: u32, listing: Listing, folder_path: &str, nav: Vec<NavItem>) -> Result<Self> {
        let surface = Surface::new(hwnd, w, h)?;
        let text = Text::new()?;
        let fonts = Fonts::new(&text)?;
        let rows = list::rows(&listing);
        let visible = list::visible_entries(&rows, h as f32);
        let mut icons: Vec<Option<ID2D1Bitmap1>> = (0..listing.entries.len()).map(|_| None).collect();
        for i in visible {
            icons[i] = bmp(&surface, &listing.entries[i].path, 16);
        }
        let nav_icons = nav.iter().map(|n| bmp(&surface, &n.path, 16)).collect();
        let tab_icon = bmp(&surface, folder_path, 16);
        Ok(Self {
            surface,
            text,
            fonts,
            w: w as f32,
            h: h as f32,
            listing,
            rows,
            icons,
            nav,
            nav_icons,
            tab_icon,
            selected: None,
            hovered: None,
        })
    }

    pub fn draw(&self) -> Result<()> {
        self.surface.begin();
        let p = Painter { s: &self.surface, t: &self.text, f: &self.fonts };
        chrome::draw(&p, self)?;
        pane::draw(&p, self)?;
        list::draw(&p, self)?;
        self.surface.end()
    }

    /// Row index under a client point, for hover and click.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        list::entry_at(&self.rows, x, y, self.w, self.h)
    }
}
