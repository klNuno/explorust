//! The Task Manager window: Mica behind a composition swap chain, a navigation rail and three
//! pages drawn with Direct2D, refreshed by a 1 s timer.

pub mod log;
mod paint;
mod sample;
mod startup;
mod theme;

use crate::model::{Col, Group, group, sort};
use sample::Sampler;
use std::cell::RefCell;
use std::collections::HashSet;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{HSTRING, w};

const CLASS: windows_core::PCWSTR = w!("explorust.TaskManager");
const TIMER: usize = 1;
/// The names worker read more version resources: rename the rows.
const WM_NAMES: u32 = WM_APP + 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Processes,
    Performance,
    Startup,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    /// The hamburger: opens or closes the rail's labels.
    Rail,
    Nav(Page),
    EndTask,
    RealTaskmgr,
    Toggle,
    /// A visible row of the processes list: a group (by lead pid) or one of its processes.
    Row(u32, Option<u32>),
    Expand(u32),
    Tile(usize),
    Startup(usize),
    /// A column header: sort by it (again: reverse).
    Sort(Col),
}

pub struct App {
    pub hwnd: HWND,
    pub surface: render::Surface,
    pub text: render::text::Text,
    pub f: paint::Fonts,
    pub light: bool,
    pub fr: bool,
    pub k: f32,
    pub size: (u32, u32),
    pub page: Page,
    pub s: Sampler,
    pub groups: Vec<Group>,
    pub sort: Option<(Col, bool)>,
    pub rail_open: bool,
    /// App icons as D2D bitmaps by exe path, made from the worker's pixels.
    pub icons: std::collections::HashMap<String, windows::Win32::Graphics::Direct2D::ID2D1Bitmap1>,
    pub expanded: HashSet<u32>,
    pub selected: Option<(u32, Option<u32>)>,
    pub scroll: f32,
    pub tile: usize,
    pub startup: Vec<startup::Entry>,
    pub startup_sel: Option<usize>,
    pub hits: Vec<(f32, f32, f32, f32, Hit)>,
    pub hover: Option<Hit>,
    first: bool,
    ticks: u64,
    cpu_at: (u64, i64),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn french() -> bool {
    unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() & 0x3ff == 0x0c }
}

impl App {
    pub fn t<'a>(&self, en: &'a str, fr: &'a str) -> &'a str {
        if self.fr { fr } else { en }
    }
}

fn client(hwnd: HWND) -> (u32, u32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
}

fn own_cpu() -> u64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        let f = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
        f(k) + f(u)
    }
}

fn wall() -> i64 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as i64
}

/// Entry of `explorust-taskman.exe`; a second start brings the running window to the front.
pub fn run() -> i32 {
    unsafe {
        if let Ok(h) = FindWindowW(CLASS, None) {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
            return 0;
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let code = window();
        CoUninitialize();
        code
    }
}

fn window() -> i32 {
    unsafe {
        let Ok(inst) = GetModuleHandleW(None) else { return 1 };
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc),
            hInstance: inst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (ww, wh) = (980.min(wa.right - wa.left), 680.min(wa.bottom - wa.top));
        let fr = french();
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            &HSTRING::from(if fr { "Gestionnaire des tâches" } else { "Task Manager" }),
            WS_OVERLAPPEDWINDOW,
            wa.left + (wa.right - wa.left - ww) / 2,
            wa.top + (wa.bottom - wa.top - wh) / 2,
            ww,
            wh,
            None,
            None,
            Some(inst.into()),
            None,
        ) else {
            return 1;
        };
        let light = theme::light();
        let dark: i32 = (!light) as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let kind = DWMSBT_MAINWINDOW;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as _, 4);
        let m = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let k = GetDpiForWindow(hwnd) as f32 / 96.0;
        let size = client(hwnd);
        let (Ok(surface), Ok(text)) = (render::Surface::new(hwnd, size.0, size.1), render::text::Text::new()) else {
            log::line("surface or text failed");
            return 1;
        };
        let f = paint::Fonts::new(&text, k);
        log::line(&format!("window and device ready {:.1} ms after process start", log::since_start()));
        let s = Sampler::new();
        log::line(&format!("first sample {:.1} ms after process start", log::since_start()));
        let groups = group(&s.procs);
        log::line(&format!("first sample {} processes", s.procs.len()));
        let app = App {
            hwnd,
            surface,
            text,
            f,
            light,
            fr,
            k,
            size,
            page: Page::Processes,
            s,
            groups,
            sort: None,
            // Open on a wide window, as Windows' Task Manager.
            rail_open: size.0 as f32 / k >= 900.0,
            icons: std::collections::HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            scroll: 0.0,
            tile: 0,
            startup: Vec::new(),
            startup_sel: None,
            hits: Vec::new(),
            hover: None,
            first: true,
            ticks: 0,
            cpu_at: (own_cpu(), wall()),
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        with(|a| a.paint());
        let target = hwnd.0 as usize;
        with(|a| {
            a.s.start_names((16.0 * a.k).round() as i32, move || {
                let _ = PostMessageW(Some(HWND(target as _)), WM_NAMES, WPARAM(0), LPARAM(0));
            })
        });
        SetTimer(Some(hwnd), TIMER, 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
        0
    }
}

impl App {
    fn tick(&mut self) {
        self.s.tick();
        self.regroup();
        self.ticks += 1;
        // Own cost every 60 s: CPU time over wall time, of one core and of the machine.
        if self.ticks % 60 == 0 {
            let (c, t) = (own_cpu(), wall());
            let (pc, pt) = self.cpu_at;
            let pct = (c - pc) as f64 / (t - pt).max(1) as f64 * 100.0;
            let mut pmc = windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX::default();
            unsafe {
                let _ = windows::Win32::System::ProcessStatus::GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc as *mut _ as _, size_of_val(&pmc) as u32);
            }
            log::line(&format!(
                "idle cost over {:.0} s: {pct:.2}% of one core, {:.2}% of {} cpus, last sample {:.1} ms, private {:.1} MB, page {:?}",
                (t - pt) as f64 / 1e7,
                pct / self.s.ncpu() as f64,
                self.s.ncpu(),
                self.s.cost_ms,
                pmc.PrivateUsage as f64 / 1048576.0,
                self.page
            ));
            self.cpu_at = (c, t);
        }
    }

    /// Groups the last sample and applies the column sort.
    fn regroup(&mut self) {
        self.groups = group(&self.s.procs);
        if let Some((col, asc)) = self.sort {
            sort(&mut self.groups, col, asc);
        }
    }

    pub fn group_of(&self, lead: u32) -> Option<&Group> {
        self.groups.iter().find(|g| self.s.procs[g.lead].pid == lead)
    }

    fn selected_pids(&self) -> Vec<u32> {
        match self.selected {
            Some((_, Some(pid))) => vec![pid],
            Some((lead, None)) => self.group_of(lead).map(|g| g.pids.clone()).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    fn end_task(&mut self) {
        for pid in self.selected_pids() {
            let r = unsafe {
                OpenProcess(PROCESS_TERMINATE, false, pid).and_then(|h| {
                    let r = TerminateProcess(h, 1);
                    let _ = CloseHandle(h);
                    r
                })
            };
            log::line(&format!("end task {pid}: {r:?}"));
        }
        self.selected = None;
        self.tick();
        self.paint();
    }

    fn open_location(&self) {
        let pid = match self.selected {
            Some((_, Some(p))) => p,
            Some((lead, None)) => lead,
            None => return,
        };
        let Some(i) = self.s.procs.iter().position(|p| p.pid == pid) else { return };
        let path = self.s.metas[i].path.clone();
        if path.is_empty() {
            return;
        }
        let r = unsafe {
            let pidl = ILCreateFromPathW(&HSTRING::from(path.as_str()));
            if pidl.is_null() {
                Err(windows_core::Error::empty())
            } else {
                let r = SHOpenFolderAndSelectItems(pidl, None, 0);
                ILFree(Some(pidl));
                r
            }
        };
        log::line(&format!("open file location {path}: {r:?}"));
    }

    fn real_taskmgr(&self) {
        let r = unsafe { ShellExecuteW(Some(self.hwnd), w!("open"), w!("taskmgr.exe"), None, None, SW_SHOWNORMAL) };
        log::line(&format!("windows task manager started: {}", r.0 as usize > 32));
    }

    fn click(&mut self, h: Hit) {
        match h {
            Hit::Rail => self.rail_open = !self.rail_open,
            Hit::Nav(p) => {
                self.page = p;
                self.scroll = 0.0;
                if p == Page::Startup {
                    self.startup = startup::list();
                    log::line(&format!("startup apps: {}", self.startup.len()));
                }
            }
            Hit::EndTask => self.end_task(),
            Hit::RealTaskmgr => self.real_taskmgr(),
            Hit::Toggle => {
                if let Some(e) = self.startup_sel.and_then(|i| self.startup.get(i)).cloned() {
                    startup::set(&e, !e.enabled);
                    self.startup = startup::list();
                }
            }
            Hit::Row(lead, pid) => self.selected = Some((lead, pid)),
            Hit::Expand(lead) => {
                if !self.expanded.remove(&lead) {
                    self.expanded.insert(lead);
                }
            }
            Hit::Tile(i) => self.tile = i,
            Hit::Startup(i) => self.startup_sel = Some(i),
            Hit::Sort(col) => {
                // Name starts A to Z, values largest first; a second click reverses.
                self.sort = match self.sort {
                    Some((c, asc)) if c == col => Some((col, !asc)),
                    _ => Some((col, col == Col::Name)),
                };
                self.regroup();
                log::line(&format!("sort {:?}", self.sort));
            }
        }
        self.paint();
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(l, t, r, b, _)| x >= *l && x < *r && y >= *t && y < *b).map(|h| h.4)
    }

    fn menu(&mut self, x: i32, y: i32) {
        unsafe {
            let Ok(m) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(m, MF_STRING, 1, &HSTRING::from(self.t("End task", "Fin de tâche")));
            let _ = AppendMenuW(m, MF_STRING, 2, &HSTRING::from(self.t("Open file location", "Ouvrir l'emplacement du fichier")));
            let mut p = POINT { x, y };
            let _ = ClientToScreen(self.hwnd, &mut p);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, None, self.hwnd, None);
            let _ = DestroyMenu(m);
            match cmd.0 {
                1 => self.end_task(),
                2 => self.open_location(),
                _ => {}
            }
        }
    }
}

impl Sampler {
    pub fn ncpu(&self) -> u32 {
        self.ncpu_pub()
    }
}

fn xy(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => {
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                FillRect(HDC(wp.0 as _), &r, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                LRESULT(1)
            }
            WM_NAMES => {
                with(|a| {
                    a.s.rename();
                    let new: Vec<(String, (u32, Vec<u8>))> = {
                        let icons = a.s.icons.lock().unwrap();
                        icons.iter().filter(|(p, _)| !a.icons.contains_key(*p)).map(|(p, v)| (p.clone(), v.clone())).collect()
                    };
                    for (p, (side, px)) in new {
                        if let Ok(b) = a.surface.bitmap(side, side, &px) {
                            a.icons.insert(p, b);
                        }
                    }
                    a.regroup();
                    a.paint();
                });
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                with(|a| a.paint());
                LRESULT(0)
            }
            WM_SIZE => {
                with(|a| {
                    a.size = client(hwnd);
                    let _ = a.surface.resize(a.size.0, a.size.1);
                    a.paint();
                });
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER => {
                with(|a| {
                    a.tick();
                    // Minimized: keep sampling for the graphs, skip drawing.
                    if !IsIconic(hwnd).as_bool() {
                        a.paint();
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let d = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                with(|a| {
                    a.scroll = (a.scroll - d * 3.0 * 28.0 * a.k).max(0.0);
                    a.paint();
                });
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = xy(lp);
                with(|a| {
                    let h = a.hit(x, y);
                    if h != a.hover {
                        a.hover = h;
                        a.paint();
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = xy(lp);
                with(|a| {
                    if let Some(h) = a.hit(x, y) {
                        a.click(h);
                    }
                });
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let (x, y) = xy(lp);
                with(|a| {
                    if let Some(h @ Hit::Row(..)) = a.hit(x, y) {
                        a.click(h);
                        a.menu(x as i32, y as i32);
                    }
                });
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = VIRTUAL_KEY(wp.0 as u16);
                with(|a| match key {
                    VK_DELETE => a.end_task(),
                    VK_ESCAPE => a.selected = None,
                    _ => {}
                });
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                with(|a| {
                    let l = theme::light();
                    if l != a.light {
                        a.light = l;
                        let dark: i32 = (!l) as i32;
                        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
                        a.paint();
                    }
                });
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn first_frame(a: &mut App) {
    if a.first {
        a.first = false;
        log::line(&format!("first frame on screen {:.1} ms after process start, {} processes", log::since_start(), a.s.procs.len()));
    }
}
