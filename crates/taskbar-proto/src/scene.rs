//! What the bar draws: Start, pinned and running apps, tray, clock.

use render::text::{self, Align, Text};
use render::{Surface, rgba};
use shelldata::layout::{self as L, TrayItem};
use std::time::Instant;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT, ID2D1Bitmap1};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, IDWriteTextFormat};
use windows_core::Result;

struct Button {
    icon: Option<ID2D1Bitmap1>,
    running: bool,
    active: bool,
    hover: f32,
}

pub struct Scene {
    surface: Surface,
    text: Text,
    width: f32,
    plate: ID2D1Bitmap1,
    start: ID2D1Bitmap1,
    buttons: Vec<Button>, // index 0 is Start
    tray_icons: Vec<(&'static str, ID2D1Bitmap1)>,
    clock_fmt: IDWriteTextFormat,
    center_fmt: IDWriteTextFormat,
    glyph_fmt: IDWriteTextFormat,
    clock: (String, String),
    hovered: Option<usize>,
    last_step: Instant,
}

fn bmp(s: &Surface, img: &shelldata::Image) -> Result<ID2D1Bitmap1> {
    s.bitmap(img.width, img.height, &img.to_premultiplied_bgra())
}

impl Scene {
    pub fn new(hwnd: HWND, width: u32) -> Result<Self> {
        let surface = Surface::new(hwnd, width, L::BAR_H as u32)?;
        let text = Text::new()?;
        let apps = shelldata::apps::taskbar_apps(L::ICON as i32);
        let mut buttons = vec![Button { icon: None, running: false, active: false, hover: 0.0 }];
        for a in &apps {
            let icon = a.icon.as_ref().and_then(|i| bmp(&surface, i).ok());
            buttons.push(Button { icon, running: a.running, active: a.active, hover: 0.0 });
        }
        for a in &apps {
            shelldata::bench::log(&format!("A app {} running={} active={} path={}", a.name, a.running, a.active, a.path));
        }
        let mut tray_icons = Vec::new();
        for (_, item) in L::TRAY {
            if let TrayItem::App(key) = item {
                if let Some(a) = apps.iter().find(|a| a.path.contains(key)) {
                    if let Some(img) = shelldata::icons::icon_for_path(&a.path, 16) {
                        tray_icons.push((*key, bmp(&surface, &img)?));
                    }
                }
            }
        }
        let plate = bmp(&surface, &L::plate_image(width, buttons.len()))?;
        let start = bmp(&surface, &L::start_image())?;
        let clock_fmt = text.format("Segoe UI Variable Text", L::TEXT_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Trailing)?;
        let center_fmt = text.format("Segoe UI Variable Text", L::TEXT_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        let glyph_fmt = text.format("Segoe Fluent Icons", L::GLYPH_PX, DWRITE_FONT_WEIGHT_NORMAL, Align::Center)?;
        Ok(Self {
            surface,
            text,
            width: width as f32,
            plate,
            start,
            buttons,
            tray_icons,
            clock_fmt,
            center_fmt,
            glyph_fmt,
            clock: shelldata::clock::now(),
            hovered: None,
            last_step: Instant::now(),
        })
    }

    pub fn refresh_clock(&mut self) {
        self.clock = shelldata::clock::now();
    }

    /// Sets the hovered button from a pointer x; true when an animation must run.
    pub fn set_hover_x(&mut self, x: Option<f32>) -> bool {
        let n = self.buttons.len();
        let idx = x.and_then(|x| {
            let rel = x - L::group_x0(self.width, n);
            (rel >= 0.0 && rel < n as f32 * L::SLOT).then(|| (rel / L::SLOT) as usize)
        });
        if idx == self.hovered {
            return false;
        }
        self.hovered = idx;
        self.last_step = Instant::now();
        true
    }

    /// Advances hover fades; true while any is still moving.
    pub fn step(&mut self) -> bool {
        let dt = self.last_step.elapsed().as_secs_f32() * 1000.0;
        self.last_step = Instant::now();
        let mut moving = false;
        for (i, b) in self.buttons.iter_mut().enumerate() {
            let target = if Some(i) == self.hovered { 1.0 } else { 0.0 };
            moving |= L::fade(&mut b.hover, target, dt);
        }
        moving
    }

    pub fn draw(&mut self) -> Result<()> {
        let s = &self.surface;
        let ctx = &s.ctx;
        s.begin();
        let white = s.brush(rgba(255, 255, 255, 1.0))?;
        unsafe {
            ctx.DrawBitmap(&self.plate, None, 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
            let n = self.buttons.len();
            for (i, b) in self.buttons.iter().enumerate() {
                let cx = L::button_cx(self.width, n, i);
                let a = if b.active { L::ACTIVE_BG_A } else { 0.0 } + b.hover * L::HOVER_BG_A;
                if a > 0.0 {
                    let (ox, y, w, h) = L::BUTTON_BG;
                    let br = s.brush(rgba(255, 255, 255, a))?;
                    let rr = D2D1_ROUNDED_RECT { rect: rect(cx + ox, y, w, h), radiusX: L::BUTTON_RADIUS, radiusY: L::BUTTON_RADIUS };
                    ctx.FillRoundedRectangle(&rr, &br);
                }
                if i == 0 {
                    let side = L::START_SQ * 2.0 + 1.0;
                    let r = rect((cx - side / 2.0).floor() + 0.5, L::ICON_TOP + 1.0, side, side);
                    ctx.DrawBitmap(&self.start, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                    continue;
                }
                if let Some(icon) = &b.icon {
                    let r = rect(cx - L::ICON / 2.0, L::ICON_TOP, L::ICON, L::ICON);
                    ctx.DrawBitmap(icon, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                }
                if b.running {
                    let (w, c) = if b.active { (L::ACTIVE_W, L::ACTIVE_RGB) } else { (L::RUNNING_W, L::RUNNING_RGB) };
                    let br = s.brush(rgba(c.0, c.1, c.2, 1.0))?;
                    let rr = D2D1_ROUNDED_RECT { rect: rect(cx - w / 2.0, L::INDICATOR_Y, w, L::INDICATOR_H), radiusX: 1.5, radiusY: 1.5 };
                    ctx.FillRoundedRectangle(&rr, &br);
                }
            }
        }
        for (off, item) in L::TRAY {
            let cx = self.width - off;
            match item {
                TrayItem::Glyph(g) => text::draw(ctx, &self.text, &self.glyph_fmt, &g.to_string(), cx - 20.0, cx + 20.0, 32.0, &white.clone().into(), false),
                TrayItem::TwoLines(a, b) => {
                    text::draw(ctx, &self.text, &self.center_fmt, a, cx - 30.0, cx + 30.0, L::LINE1_BASELINE, &white.clone().into(), false);
                    text::draw(ctx, &self.text, &self.center_fmt, b, cx - 30.0, cx + 30.0, L::LINE2_BASELINE, &white.clone().into(), false);
                }
                TrayItem::App(key) => {
                    if let Some((_, icon)) = self.tray_icons.iter().find(|(k, _)| k == key) {
                        let r = rect(cx - 8.0, 16.0, 16.0, 16.0);
                        unsafe { ctx.DrawBitmap(icon, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
                    }
                }
            }
        }
        let right = self.width - L::CLOCK_RIGHT;
        text::draw(ctx, &self.text, &self.clock_fmt, &self.clock.0, right - 100.0, right, L::LINE1_BASELINE, &white.clone().into(), true);
        text::draw(ctx, &self.text, &self.clock_fmt, &self.clock.1, right - 100.0, right, L::LINE2_BASELINE, &white.into(), true);
        s.end()
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}
