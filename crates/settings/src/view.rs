use render::style::{Document, Material, Part, Style};
use render::text::{Align, Text};
use render::{Color, Surface, rgba};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_core::Result;

mod appearance;

pub const TOP: f32 = 120.0;
const ROW: f32 = 55.0;
const FOOTER: f32 = 38.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Recovery,
    About,
}

impl Page {
    pub const ALL: [Self; 3] = [Self::Appearance, Self::Recovery, Self::About];

    fn label(self, fr: bool) -> &'static str {
        match self {
            Self::Appearance => tr(fr, "Appearance", "Apparence"),
            Self::Recovery => tr(fr, "Startup & recovery", "Démarrage et retour"),
            Self::About => tr(fr, "About", "À propos"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "\u{E771}",
            Self::Recovery => "\u{E777}",
            Self::About => "\u{E946}",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Page(Page),
    Choose(Option<Part>),
    Material(Material),
    Opacity(Part),
    Reset,
    Fallback,
    Uninstall,
}

#[derive(Clone, Copy)]
pub struct Hit {
    pub action: Action,
    pub rect: D2D_RECT_F,
    pub scrolls: bool,
}

impl Hit {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.rect.left && x < self.rect.right && y >= self.rect.top && y < self.rect.bottom
    }
}

pub struct Model {
    pub doc: Document,
    pub page: Page,
    pub hovered: Option<Action>,
    pub focused: Option<Action>,
    pub dropdown: Option<Option<Part>>,
    pub scroll: f32,
    pub fr: bool,
    pub error: bool,
}

pub fn tr(fr: bool, en: &'static str, french: &'static str) -> &'static str {
    if fr { french } else { en }
}

fn material_label(material: Material, fr: bool) -> &'static str {
    match material {
        Material::Windows => "Windows",
        Material::Acrylic => tr(fr, "Acrylic", "Acrylique"),
        Material::Transparent => "Transparent",
        Material::Mica => "Mica",
        Material::Oled => tr(fr, "OLED black", "Noir OLED"),
    }
}

pub fn available(part: Part) -> bool {
    part != Part::QuickSettings
}

fn part_label(part: Part, fr: bool) -> &'static str {
    match part {
        Part::Taskbar => tr(fr, "Taskbar", "Barre des tâches"),
        Part::Start => tr(fr, "Start menu", "Menu Démarrer"),
        Part::Notifications => "Notifications",
        Part::QuickSettings => tr(fr, "Quick settings", "Paramètres rapides"),
        Part::Files => tr(fr, "File Explorer", "Explorateur de fichiers"),
        Part::Menus => tr(fr, "Context menus", "Menus contextuels"),
        Part::Switcher => "Alt+Tab",
    }
}

pub struct Scene {
    pub surface: Surface,
    ui: IDWriteTextFormat,
    strong: IDWriteTextFormat,
    small: IDWriteTextFormat,
    title: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    ink: ID2D1SolidColorBrush,
    logo: Option<ID2D1Bitmap1>,
    logo_small: Option<ID2D1Bitmap1>,
}

const TEXT: Color = rgba(245, 246, 253, 1.0);
const MUTED: Color = rgba(195, 200, 218, 1.0);
const ACCENT: Color = rgba(163, 185, 255, 1.0);
const CARD: Color = rgba(255, 255, 255, 0.048);
const BORDER: Color = rgba(255, 255, 255, 0.14);
const CONTROL: Color = rgba(255, 255, 255, 0.075);
const HOVER: Color = rgba(255, 255, 255, 0.14);
const SIDEBAR: Color = rgba(14, 19, 36, 0.68);

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

pub fn max_scroll(height: f32) -> f32 {
    (TOP + ROW * (Part::ALL.len() + 1) as f32 + 16.0 + FOOTER - height).max(0.0)
}

impl Scene {
    pub fn new(surface: Surface, exe: &str) -> Result<Self> {
        let text = Text::new()?;
        let family = "Segoe UI Variable Text";
        let format = |family, size, weight| text.format(family, size, weight, Align::Leading);
        let load_logo = |size| {
            shelldata::icons::icon_for_path(exe, size).and_then(|i| {
                surface
                    .bitmap(i.width, i.height, &i.to_premultiplied_bgra())
                    .ok()
            })
        };
        Ok(Self {
            ui: format(family, 14.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            strong: format(family, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            small: format(family, 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            title: format(
                "Segoe UI Variable Display",
                32.0,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
            )?,
            glyph: format("Segoe Fluent Icons", 16.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            ink: surface.brush(TEXT)?,
            logo: load_logo(256),
            logo_small: load_logo(16),
            surface,
        })
    }

    fn box_(&self, r: D2D_RECT_F, radius: f32, fill: Color, border: Option<Color>) {
        let shape = D2D1_ROUNDED_RECT {
            rect: r,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.ink.SetColor(&fill);
            self.surface.ctx.FillRoundedRectangle(&shape, &self.ink);
            if let Some(border) = border {
                self.ink.SetColor(&border);
                self.surface
                    .ctx
                    .DrawRoundedRectangle(&shape, &self.ink, 1.0, None);
            }
        }
    }

    fn text(&self, value: &str, font: &IDWriteTextFormat, r: D2D_RECT_F, color: Color) {
        let value: Vec<_> = value.encode_utf16().collect();
        unsafe {
            self.ink.SetColor(&color);
            self.surface.ctx.DrawText(
                &value,
                font,
                &r,
                &self.ink,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn icon(&self, name: &str, x: f32, y: f32, color: Color) {
        self.text(name, &self.glyph, rect(x, y, 24.0, 24.0), color);
    }

    fn logo(&self, r: D2D_RECT_F, small: bool) {
        if let Some(logo) = if small { &self.logo_small } else { &self.logo } {
            unsafe {
                self.surface.ctx.DrawBitmap(
                    logo,
                    Some(&r),
                    1.0,
                    D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                )
            };
        }
    }

    fn control(&self, m: &Model, r: D2D_RECT_F, action: Action) {
        let fill = if m.hovered == Some(action) {
            HOVER
        } else {
            CONTROL
        };
        self.box_(
            r,
            6.0,
            fill,
            Some(if m.focused == Some(action) {
                ACCENT
            } else {
                BORDER
            }),
        );
    }

    fn swatch(&self, x: f32, y: f32, s: Style) {
        let r = rect(x, y, 32.0, 28.0);
        let base = match s.material {
            Material::Windows => rgba(68, 74, 93, 1.0),
            Material::Acrylic => rgba(94, 112, 153, 0.45 + s.opacity as f32 * 0.005),
            Material::Transparent => rgba(125, 152, 206, 0.08 + s.opacity as f32 * 0.007),
            Material::Mica => rgba(58, 67, 95, 1.0),
            Material::Oled => rgba(0, 0, 0, 1.0),
        };
        if s.material == Material::Transparent {
            self.box_(r, 6.0, rgba(39, 53, 84, 1.0), None);
            self.box_(
                rect(x + 8.0, y + 6.0, 16.0, 16.0),
                3.0,
                rgba(151, 175, 255, 0.65),
                None,
            );
        }
        self.box_(r, 6.0, base, Some(BORDER));
        if matches!(s.material, Material::Acrylic | Material::Mica) {
            self.box_(
                rect(x + 5.0, y + 5.0, 22.0, 8.0),
                3.0,
                rgba(182, 202, 255, 0.17),
                None,
            );
        }
    }

    pub fn draw(&self, m: &Model, w: f32, h: f32) -> Result<Vec<Hit>> {
        let side = if w < 865.0 { 196.0 } else { 218.0 };
        let left = side + 28.0;
        let right = w - 28.0;
        let width = (right - left).max(1.0);
        let mut hits = Vec::new();
        self.surface.begin();
        self.box_(rect(0.0, 0.0, w, h), 0.0, rgba(19, 23, 36, 0.90), None);
        self.box_(rect(0.0, 0.0, side, h), 0.0, SIDEBAR, None);
        self.box_(
            rect(side, 0.0, 1.0, h),
            0.0,
            rgba(255, 255, 255, 0.11),
            None,
        );
        self.logo(rect(20.0, 26.0, 34.0, 34.0), false);
        self.text(
            "explorust",
            &self.strong,
            rect(62.0, 32.0, side - 82.0, 30.0),
            TEXT,
        );
        for (i, page) in Page::ALL.into_iter().enumerate() {
            let y = 94.0 + i as f32 * 45.0;
            let r = rect(12.0, y, side - 24.0, 38.0);
            let action = Action::Page(page);
            let selected = page == m.page;
            if selected || m.hovered == Some(action) || m.focused == Some(action) {
                self.box_(
                    r,
                    7.0,
                    if selected {
                        rgba(145, 169, 255, 0.17)
                    } else {
                        CONTROL
                    },
                    (m.focused == Some(action)).then_some(ACCENT),
                );
            }
            self.icon(
                page.icon(),
                25.0,
                y + 10.0,
                if selected { ACCENT } else { MUTED },
            );
            self.text(
                page.label(m.fr),
                &self.small,
                rect(51.0, y + 11.0, side - 58.0, 24.0),
                TEXT,
            );
            hits.push(Hit {
                action,
                rect: r,
                scrolls: false,
            });
        }
        let title = if m.page == Page::Recovery {
            tr(
                m.fr,
                "Startup & return to Windows",
                "Démarrage et retour à Windows",
            )
        } else {
            m.page.label(m.fr)
        };
        self.text(title, &self.title, rect(left, 28.0, width, 52.0), TEXT);
        match m.page {
            Page::Appearance => self.appearance(m, left, width, h, &mut hits),
            Page::Recovery => self.recovery(m, left, width, &mut hits),
            Page::About => self.about(m, left, width, h),
        }
        self.surface.end()?;
        Ok(hits)
    }

    fn recovery(&self, m: &Model, x: f32, width: f32, hits: &mut Vec<Hit>) {
        self.text(
            tr(
                m.fr,
                "explorust starts when you sign in to Windows.",
                "explorust démarre à votre ouverture de session Windows.",
            ),
            &self.ui,
            rect(x, 77.0, width, 31.0),
            MUTED,
        );
        let actions = [
            (
                Action::Fallback,
                "\u{E777}",
                tr(
                    m.fr,
                    "Return to Windows Explorer for this session",
                    "Revenir à l’explorateur Windows pour cette session",
                ),
                tr(
                    m.fr,
                    "explorust returns the next time you sign in.",
                    "explorust revient à votre prochaine ouverture de session.",
                ),
            ),
            (
                Action::Uninstall,
                "\u{E74D}",
                tr(m.fr, "Uninstall explorust", "Désinstaller explorust"),
                tr(
                    m.fr,
                    "Remove explorust and restore Windows Explorer.",
                    "Supprimer explorust et rétablir l’explorateur Windows.",
                ),
            ),
        ];
        for (i, (action, glyph, label, hint)) in actions.into_iter().enumerate() {
            let y = 120.0 + i as f32 * 87.0;
            let r = rect(x, y, width, 75.0);
            self.control(m, r, action);
            self.icon(glyph, x + 18.0, y + 23.0, ACCENT);
            self.text(
                label,
                &self.strong,
                rect(x + 48.0, y + 14.0, width - 85.0, 25.0),
                TEXT,
            );
            self.text(
                hint,
                &self.ui,
                rect(x + 48.0, y + 41.0, width - 85.0, 24.0),
                MUTED,
            );
            self.icon("\u{E76C}", r.right - 28.0, y + 25.0, MUTED);
            hits.push(Hit {
                action,
                rect: r,
                scrolls: false,
            });
        }
    }

    fn about(&self, m: &Model, x: f32, width: f32, h: f32) {
        let logo_size = 256.0f32.min((h - 165.0).max(32.0));
        let hero = rect(x, 120.0, width, logo_size + 28.0);
        self.box_(hero, 10.0, rgba(134, 161, 255, 0.09), Some(BORDER));
        let logo_x = x + (width - logo_size) * 0.5;
        self.logo(rect(logo_x, 134.0, logo_size, logo_size), false);
        let tiny = rect(hero.right - 46.0, hero.bottom - 46.0, 32.0, 32.0);
        self.box_(tiny, 6.0, CARD, Some(BORDER));
        self.logo(rect(tiny.left + 8.0, tiny.top + 8.0, 16.0, 16.0), true);
        self.text(
            &format!("explorust {}", env!("CARGO_PKG_VERSION")),
            &self.strong,
            rect(x, hero.bottom + 16.0, width, 27.0),
            TEXT,
        );
        self.text(
            tr(
                m.fr,
                "A Windows shell written in Rust.",
                "Un shell Windows écrit en Rust.",
            ),
            &self.ui,
            rect(x, hero.bottom + 44.0, width, 26.0),
            MUTED,
        );
    }
}
