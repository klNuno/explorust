use crate::view::{self, Action, Hit, Model, Page, Scene, tr};
use render::style::{self, Document, Material, Part, Style};
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::Command;
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Graphics::Dwm::{DWMWA_COLOR_DEFAULT, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, PCWSTR, Result, w};

mod input;
use input::window_proc;

const CLASS: PCWSTR = w!("explorust.Settings");
const TIMER_SAVE: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MIN_WIDTH: i32 = 800;
const MIN_HEIGHT: i32 = 440;

thread_local! {
    static UI: RefCell<Option<Window>> = const { RefCell::new(None) };
}

struct Window {
    hwnd: HWND,
    scale: f32,
    px: SIZE,
    model: Model,
    scene: Scene,
    hits: Vec<Hit>,
    pressed: Option<Action>,
    dragging: Option<Hit>,
    pending_opacity: Option<(Part, u8)>,
    tracking: bool,
    exe: PathBuf,
}

fn with<R>(f: impl FnOnce(&mut Window) -> R) -> Option<R> {
    UI.with(|ui| ui.try_borrow_mut().ok()?.as_mut().map(f))
}

fn french() -> bool {
    unsafe { GetUserDefaultUILanguage() & 0x3ff == 0x0c }
}

pub fn report_error(detail: &str) {
    let title = tr(french(), "explorust settings", "Paramètres d’explorust");
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(detail),
            &HSTRING::from(title),
            MB_ICONERROR | MB_OK,
        )
    };
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Session(HANDLE);
impl Drop for Session {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn run() -> Result<()> {
    unsafe {
        let mutex = CreateMutexW(None, false, w!(r"Local\explorust-settings"))?;
        let duplicate = GetLastError() == ERROR_ALREADY_EXISTS;
        let _session = Session(mutex);
        if duplicate {
            if let Ok(existing) = FindWindowW(CLASS, None) {
                if IsIconic(existing).as_bool() {
                    let _ = ShowWindow(existing, SW_RESTORE);
                }
                let _ = SetForegroundWindow(existing);
            }
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _com = Com;
        let fr = french();
        let exe = std::env::current_exe()?;
        let instance = GetModuleHandleW(None)?;
        let icon_id = PCWSTR(1usize as *const u16);
        let large = LoadIconW(Some(instance.into()), icon_id).unwrap_or_default();
        let small = LoadImageW(
            Some(instance.into()),
            icon_id,
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        )
        .unwrap_or_default();
        let title = HSTRING::from(tr(fr, "explorust settings", "Paramètres d’explorust"));
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: large,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            &title,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            928,
            685,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(small.0 as isize)),
        );
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let area = info.rcWork;
            let width = ((928.0 * scale) as i32).min(area.right - area.left);
            let height = ((685.0 * scale) as i32).min(area.bottom - area.top);
            let x = area.left + (area.right - area.left - width) / 2;
            let y = area.top + (area.bottom - area.top - height) / 2;
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        style::apply(
            hwnd,
            style::WindowStyle {
                native: style::Native::Mica,
                border: DWMWA_COLOR_DEFAULT,
                corner: DWMWCP_ROUND,
                popup: true,
            },
            Part::Files.defaults(),
        );
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let px = client_size(hwnd);
        let surface = render::Surface::new(hwnd, px.cx.max(1) as u32, px.cy.max(1) as u32)?;
        surface.ctx.SetDpi(96.0 * scale, 96.0 * scale);
        let scene = Scene::new(surface, &exe.to_string_lossy())?;
        let doc = Document::load();
        let model = Model {
            page: Page::Appearance,
            hovered: None,
            focused: None,
            dropdown: None,
            scroll: 0.0,
            fr,
            error: doc.is_err(),
            doc: doc.unwrap_or_default(),
        };
        UI.with(|slot| {
            *slot.borrow_mut() = Some(Window {
                hwnd,
                model,
                scene,
                scale,
                px,
                hits: Vec::new(),
                pressed: None,
                dragging: None,
                pending_opacity: None,
                tracking: false,
                exe,
            })
        });
        with(Window::redraw);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, None, 0, 0).0 {
                -1 => return Err(windows_core::Error::from_thread()),
                0 => break,
                _ => {
                    DispatchMessageW(&message);
                }
            }
        }
        UI.with(|slot| slot.borrow_mut().take());
    }
    Ok(())
}

fn client_size(hwnd: HWND) -> SIZE {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    SIZE {
        cx: r.right,
        cy: r.bottom,
    }
}

impl Window {
    fn height(&self) -> f32 {
        self.px.cy as f32 / self.scale
    }

    fn redraw(&mut self) {
        if self.px.cx > 0 && self.px.cy > 0 {
            if let Ok(hits) =
                self.scene
                    .draw(&self.model, self.px.cx as f32 / self.scale, self.height())
            {
                self.hits = hits;
            }
        }
    }

    fn resized(&mut self) {
        let px = client_size(self.hwnd);
        if px.cx == 0 || px.cy == 0 {
            return;
        }
        let scale = unsafe { GetDpiForWindow(self.hwnd) }.max(96) as f32 / 96.0;
        if self.px != px {
            if self
                .scene
                .surface
                .resize(px.cx as u32, px.cy as u32)
                .is_err()
            {
                return;
            }
            self.px = px;
        }
        self.scale = scale;
        unsafe { self.scene.surface.ctx.SetDpi(96.0 * scale, 96.0 * scale) };
        self.model.scroll = self.model.scroll.min(view::max_scroll(self.height()));
        self.model.dropdown = None;
        self.redraw();
    }

    fn commit(&mut self, edit: impl FnOnce(&mut Document)) {
        let result = Document::load().and_then(|mut doc| {
            edit(&mut doc);
            doc.save()?;
            Ok(doc)
        });
        self.after_save(result);
    }

    fn after_save(&mut self, result: std::io::Result<Document>) {
        self.model.error = result.is_err();
        if let Ok(doc) = result {
            self.model.doc = doc;
        } else if let Ok(doc) = Document::load() {
            self.model.doc = doc;
        }
        self.redraw();
    }

    fn flush_opacity(&mut self) {
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_SAVE) };
        if let Some((part, opacity)) = self.pending_opacity.take() {
            self.commit(|doc| {
                doc.set(
                    part,
                    Style {
                        opacity,
                        ..doc.get(part)
                    },
                )
            });
        }
    }

    fn opacity(&mut self, part: Part, opacity: u8) {
        let style = self.model.doc.get(part);
        if opacity == style.opacity {
            return;
        }
        self.model.doc.set(part, Style { opacity, ..style });
        if self.pending_opacity.is_none()
            && unsafe { SetTimer(Some(self.hwnd), TIMER_SAVE, 60, None) } == 0
        {
            self.pending_opacity = Some((part, opacity));
            self.flush_opacity();
            return;
        }
        self.pending_opacity = Some((part, opacity));
        self.redraw();
    }
}
