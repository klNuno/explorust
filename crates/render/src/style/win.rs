use super::{Document, Material, Part, Style, path};
use crate::{backdrop, rgba};
use std::sync::{Mutex, Once, OnceLock};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetPropW, PostMessageW, RegisterWindowMessageW, RemovePropW, SetPropW, WM_NCDESTROY,
};
use windows_core::{HSTRING, PCWSTR, w};

const PROPERTY: PCWSTR = w!("explorust.StyleBinding");

#[derive(Clone, Copy)]
pub enum Native {
    Acrylic(u32),
    Mica,
    /// The surface paints its own default background. The color supplies the optional tint.
    Solid(u32),
}

#[derive(Clone, Copy)]
pub struct WindowStyle {
    pub native: Native,
    /// BGR color or DWMWA_COLOR_NONE. Mica frames may keep the system border.
    pub border: u32,
    pub corner: DWM_WINDOW_CORNER_PREFERENCE,
    /// Full-client DWM glass for popups. A framed window sets its own margins.
    pub popup: bool,
}

impl WindowStyle {
    pub const fn popup(tint: u32, border: u32) -> Self {
        Self {
            native: Native::Acrylic(tint),
            border,
            corner: DWMWCP_ROUND,
            popup: true,
        }
    }
}

struct Binding {
    part: Part,
    window: WindowStyle,
    applied: Style,
    repaint: Option<fn(HWND)>,
}

struct State {
    styles: [Style; Part::ALL.len()],
    windows: Vec<(isize, Part)>,
}

impl State {
    fn read(doc: &Document) -> [Style; Part::ALL.len()] {
        Part::ALL.map(|p| doc.get(p))
    }
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(State {
            styles: State::read(&Document::load().unwrap_or_default()),
            windows: Vec::new(),
        })
    })
}

pub fn current(part: Part) -> Style {
    state().lock().unwrap().styles[part as usize]
}

fn message() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("explorust.StyleChanged")) })
}

fn refresh() {
    let Ok(doc) = Document::load() else { return };
    let mut state = state().lock().unwrap();
    let styles = State::read(&doc);
    for &(hwnd, part) in &state.windows {
        if styles[part as usize] != state.styles[part as usize] {
            // The subscription and WM_NCDESTROY use this mutex. The HWND is still live.
            let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), message(), WPARAM(0), LPARAM(0)) };
        }
    }
    state.styles = styles;
}

fn watch() {
    let Ok(path) = path() else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Watch changes by name. The shell frequently writes logs in the same directory.
    unsafe {
        let Ok(directory) = CreateFileW(
            &HSTRING::from(dir.as_os_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            None,
        ) else {
            return;
        };
        let mut buffer = [0u32; 4096];
        let mut needs_refresh = true;
        loop {
            let mut request = OVERLAPPED::default();
            if ReadDirectoryChangesW(
                directory,
                buffer.as_mut_ptr().cast(),
                size_of_val(&buffer) as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
                None,
                Some(&mut request),
                None,
            )
            .is_err()
            {
                break;
            }
            if needs_refresh {
                refresh();
            }
            let mut size = 0;
            if GetOverlappedResult(directory, &request, &mut size, true).is_err() {
                break;
            }
            let bytes = std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), size as usize);
            needs_refresh = style_changed(bytes);
        }
        let _ = CloseHandle(directory);
    }
}

fn style_changed(mut bytes: &[u8]) -> bool {
    const NAME: &[u8] = b"style.json";
    if bytes.is_empty() {
        return true;
    } // The kernel's change buffer overflowed.
    while bytes.len() >= 12 {
        let next = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if size == NAME.len() * 2
            && bytes.get(12..12 + size).is_some_and(|name| {
                name.chunks_exact(2)
                    .zip(NAME)
                    .all(|(wide, &ascii)| wide[1] == 0 && wide[0].eq_ignore_ascii_case(&ascii))
            })
        {
            return true;
        }
        if next < 12 || next > bytes.len() {
            break;
        }
        bytes = &bytes[next..];
    }
    false
}

/// Applies styles and invalidates this window on changes. Call only on its owning thread.
pub fn bind(hwnd: HWND, part: Part, window: WindowStyle) {
    bind_with_repaint(hwnd, part, window, None);
}

pub fn bind_with_repaint(hwnd: HWND, part: Part, window: WindowStyle, repaint: Option<fn(HWND)>) {
    const ID: usize = 0x7374_796C;
    // Subclass data belongs to the UI thread, and is released on WM_NCDESTROY.
    unsafe {
        let existing = GetPropW(hwnd, PROPERTY).0.cast::<Binding>();
        if !existing.is_null() {
            let applied = current(part);
            (*existing).window = window;
            (*existing).applied = applied;
            (*existing).repaint = repaint;
            apply(hwnd, window, applied);
            return;
        }
        let mut binding = Box::new(Binding {
            part,
            window,
            applied: part.defaults(),
            repaint,
        });
        let raw = &mut *binding as *mut Binding;
        if SetPropW(hwnd, PROPERTY, Some(HANDLE(raw.cast()))).is_err() {
            return;
        }
        if !SetWindowSubclass(hwnd, Some(subclass), ID, raw as usize).as_bool() {
            let _ = RemovePropW(hwnd, PROPERTY);
            return;
        }
        {
            let mut state = state().lock().unwrap();
            state.windows.push((hwnd.0 as isize, part));
            binding.applied = state.styles[part as usize];
        }
        let applied = binding.applied;
        let _ = Box::into_raw(binding);
        apply(hwnd, window, applied);
    }
    static WATCH: Once = Once::new();
    WATCH.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("appearance".into())
            .spawn(watch);
    });
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    raw: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_NCDESTROY {
            state()
                .lock()
                .unwrap()
                .windows
                .retain(|(h, _)| *h != hwnd.0 as isize);
            let _ = RemovePropW(hwnd, PROPERTY);
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), id);
            drop(Box::from_raw(raw as *mut Binding));
        } else if msg == message() && msg != 0 {
            let binding = &mut *(raw as *mut Binding);
            let new = current(binding.part);
            if new != binding.applied {
                binding.applied = new;
                let (window, repaint) = (binding.window, binding.repaint);
                apply(hwnd, window, new);
                if let Some(repaint) = repaint {
                    repaint(hwnd);
                } else {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

/// Native accent and Mica policies are mutually exclusive. Clear the old one during transitions.
pub fn apply(hwnd: HWND, frame: WindowStyle, style: Style) {
    let native_tint = match frame.native {
        Native::Acrylic(tint) | Native::Solid(tint) => tint,
        Native::Mica => 0xDB20_2020,
    };
    let rgb = native_tint & 0x00FF_FFFF;
    let tinted = rgb | ((u32::from(style.opacity.min(100)) * 255 / 100).max(1) << 24);
    let (accent, tint, backdrop) = match (style.material, frame.native) {
        (Material::Windows, Native::Acrylic(tint)) => (4, tint, DWMSBT_NONE),
        (Material::Windows, Native::Mica) | (Material::Mica, _) => (0, 0, DWMSBT_MAINWINDOW),
        (Material::Acrylic, _) => (4, tinted, DWMSBT_NONE),
        (Material::Transparent, _) => (2, tinted, DWMSBT_NONE),
        (Material::Windows, Native::Solid(_)) | (Material::Oled, _) => (0, 0, DWMSBT_NONE),
    };
    // A window's own frame can extend only as far as its custom chrome.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const frame.corner).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&raw const frame.border).cast(),
            4,
        );
        if frame.popup {
            let margin = if backdrop == DWMSBT_MAINWINDOW { -1 } else { 0 };
            let m = MARGINS {
                cxLeftWidth: margin,
                cxRightWidth: margin,
                cyTopHeight: margin,
                cyBottomHeight: margin,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        }
        if backdrop == DWMSBT_MAINWINDOW {
            let dark = 1i32;
            backdrop::accent(hwnd, 0, 0);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&raw const dark).cast(),
                4,
            );
        }
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const backdrop).cast(),
            4,
        );
        if backdrop != DWMSBT_MAINWINDOW {
            backdrop::accent(hwnd, accent, tint);
        }
    }
}

/// An opaque black composition layer gives OLED mode the same #000000 on every Windows theme.
pub fn paint_base(ctx: &ID2D1DeviceContext, part: Part) {
    if current(part).material == Material::Oled {
        unsafe { ctx.Clear(Some(&rgba(0, 0, 0, 1.0))) };
    }
}
