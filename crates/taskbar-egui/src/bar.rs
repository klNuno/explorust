//! The bar drawn with egui's painter.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, TextureHandle, pos2, vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use shelldata::layout::{self as L, TrayItem};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

struct Button {
    icon: Option<TextureHandle>,
    running: bool,
    active: bool,
    hover: f32,
}

pub struct Bar {
    width: f32,
    plate: TextureHandle,
    start: TextureHandle,
    buttons: Vec<Button>,
    tray_icons: Vec<(&'static str, TextureHandle)>,
    clock: (String, String),
    clock_at: Instant,
    frames: u32,
    last: Instant,
    quit_at: Instant,
}

fn tex(ctx: &egui::Context, name: &str, img: &shelldata::Image) -> TextureHandle {
    let ci = egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.to_rgba());
    ctx.load_texture(name, ci, egui::TextureOptions::LINEAR)
}

fn load_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::empty();
    for (key, file) in [("ui", r"C:\Windows\Fonts\SegUIVar.ttf"), ("icons", r"C:\Windows\Fonts\SegoeIcons.ttf")] {
        if let Ok(bytes) = std::fs::read(file) {
            defs.font_data.insert(key.into(), Arc::new(egui::FontData::from_owned(bytes)));
            defs.families.insert(FontFamily::Name(key.into()), vec![key.into()]);
        }
    }
    defs.families.insert(FontFamily::Proportional, vec!["ui".into()]);
    defs.families.insert(FontFamily::Monospace, vec!["ui".into()]);
    ctx.set_fonts(defs);
}

impl Bar {
    pub fn new(cc: &eframe::CreationContext<'_>, width: f32, secs: u64, bench: bool) -> Self {
        if let Ok(h) = cc.window_handle() {
            if let RawWindowHandle::Win32(w) = h.as_raw() {
                let hwnd = HWND(w.hwnd.get() as _);
                unsafe {
                    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize);
                }
                render::backdrop::apply(hwnd, render::backdrop::Backdrop::Acrylic { tint_abgr: L::TINT_ABGR });
            }
        }
        let ctx = &cc.egui_ctx;
        load_fonts(ctx);
        let apps = shelldata::apps::taskbar_apps(L::ICON as i32);
        let mut buttons = vec![Button { icon: None, running: false, active: false, hover: 0.0 }];
        for (i, a) in apps.iter().enumerate() {
            let icon = a.icon.as_ref().map(|img| tex(ctx, &format!("app{i}"), img));
            buttons.push(Button { icon, running: a.running, active: a.active, hover: 0.0 });
        }
        let mut tray_icons = Vec::new();
        for (_, item) in L::TRAY {
            if let TrayItem::App(key) = item {
                if let Some(a) = apps.iter().find(|a| a.path.contains(key)) {
                    if let Some(img) = shelldata::icons::icon_for_path(&a.path, 16) {
                        tray_icons.push((*key, tex(ctx, key, &img)));
                    }
                }
            }
        }
        let plate = tex(ctx, "plate", &L::plate_image(width as u32, buttons.len()));
        let start = tex(ctx, "start", &L::start_image());
        if bench {
            let c = ctx.clone();
            shelldata::bench::spawn_idle_bench("B", move || {
                c.send_viewport_cmd(egui::ViewportCommand::Close);
                c.request_repaint();
            });
        }
        Self {
            width,
            plate,
            start,
            buttons,
            tray_icons,
            clock: shelldata::clock::now(),
            clock_at: Instant::now(),
            frames: 0,
            last: Instant::now(),
            quit_at: Instant::now() + Duration::from_secs(secs),
        }
    }
}

impl eframe::App for Bar {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        if self.frames == 2 {
            shelldata::bench::log(&format!("B first_frame_ms={:.1}", shelldata::bench::ms_since_process_start()));
        }
        if Instant::now() >= self.quit_at {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if self.clock_at.elapsed() >= Duration::from_millis(shelldata::clock::ms_to_next_minute().min(60_000)) {
            self.clock = shelldata::clock::now();
            self.clock_at = Instant::now();
        }
        let dt = self.last.elapsed().as_secs_f32() * 1000.0;
        self.last = Instant::now();
        let n = self.buttons.len();
        let hovered = ctx.pointer_hover_pos().and_then(|p| {
            let rel = p.x - L::group_x0(self.width, n);
            (rel >= 0.0 && rel < n as f32 * L::SLOT).then(|| (rel / L::SLOT) as usize)
        });
        let mut moving = false;
        for (i, b) in self.buttons.iter_mut().enumerate() {
            moving |= L::fade(&mut b.hover, if Some(i) == hovered { 1.0 } else { 0.0 }, dt.min(50.0));
        }
        let p = ui.painter();
        let full = Rect::from_min_size(Pos2::ZERO, vec2(self.width, L::BAR_H));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        p.image(self.plate.id(), full, uv, Color32::WHITE);
        for (i, b) in self.buttons.iter().enumerate() {
            let cx = L::button_cx(self.width, n, i);
            let a = if b.active { L::ACTIVE_BG_A } else { 0.0 } + b.hover * L::HOVER_BG_A;
            if a > 0.0 {
                let (ox, y, w, h) = L::BUTTON_BG;
                p.rect_filled(Rect::from_min_size(pos2(cx + ox, y), vec2(w, h)), CornerRadius::same(L::BUTTON_RADIUS as u8), Color32::from_white_alpha((a * 255.0) as u8));
            }
            if i == 0 {
                let side = L::START_SQ * 2.0 + 1.0;
                let r = Rect::from_min_size(pos2((cx - side / 2.0).floor() + 0.5, L::ICON_TOP + 1.0), vec2(side, side));
                p.image(self.start.id(), r, uv, Color32::WHITE);
                continue;
            }
            if let Some(icon) = &b.icon {
                p.image(icon.id(), Rect::from_min_size(pos2(cx - L::ICON / 2.0, L::ICON_TOP), vec2(L::ICON, L::ICON)), uv, Color32::WHITE);
            }
            if b.running {
                let (w, c) = if b.active { (L::ACTIVE_W, L::ACTIVE_RGB) } else { (L::RUNNING_W, L::RUNNING_RGB) };
                p.rect_filled(Rect::from_min_size(pos2(cx - w / 2.0, L::INDICATOR_Y), vec2(w, L::INDICATOR_H)), CornerRadius::same(2), Color32::from_rgb(c.0, c.1, c.2));
            }
        }
        let ui_font = FontId::new(L::TEXT_PX, FontFamily::Name("ui".into()));
        let icon_font = FontId::new(L::GLYPH_PX, FontFamily::Name("icons".into()));
        // egui places text by its row box; this offset puts the baseline where DirectWrite does.
        let base = |y: f32| y - L::TEXT_PX * 0.93;
        for (off, item) in L::TRAY {
            let cx = self.width - off;
            match item {
                TrayItem::Glyph(g) => {
                    p.text(pos2(cx, L::BAR_H / 2.0), Align2::CENTER_CENTER, g, icon_font.clone(), Color32::WHITE);
                }
                TrayItem::TwoLines(a, b) => {
                    p.text(pos2(cx, base(L::LINE1_BASELINE)), Align2::CENTER_TOP, a, ui_font.clone(), Color32::WHITE);
                    p.text(pos2(cx, base(L::LINE2_BASELINE)), Align2::CENTER_TOP, b, ui_font.clone(), Color32::WHITE);
                }
                TrayItem::App(key) => {
                    if let Some((_, t)) = self.tray_icons.iter().find(|(k, _)| k == key) {
                        p.image(t.id(), Rect::from_min_size(pos2(cx - 8.0, 16.0), vec2(16.0, 16.0)), uv, Color32::WHITE);
                    }
                }
            }
        }
        let right = self.width - L::CLOCK_RIGHT;
        p.text(pos2(right, base(L::LINE1_BASELINE)), Align2::RIGHT_TOP, &self.clock.0, ui_font.clone(), Color32::WHITE);
        p.text(pos2(right, base(L::LINE2_BASELINE)), Align2::RIGHT_TOP, &self.clock.1, ui_font, Color32::WHITE);
        if moving {
            ctx.request_repaint();
        } else if self.frames == 1 {
            ctx.request_repaint();
        } else {
            let to_quit = self.quit_at.saturating_duration_since(Instant::now());
            ctx.request_repaint_after(Duration::from_millis(shelldata::clock::ms_to_next_minute()).min(to_quit));
        }
    }
}
