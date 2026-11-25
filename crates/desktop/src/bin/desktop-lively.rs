//! desktop-lively.exe: a guest test that does what Lively Wallpaper does to put its window behind
//! the desktop icons (`WinDesktopCore.cs`, `SetupDesktopLayer` and `TryAttachToDesktop`): send
//! 0x052C to Progman, find the WorkerW, parent a colored window there, keep it `secs` seconds.
//! `desktop-lively [progman|workerw] [secs] [RRGGBB] [gdi|flip|composition]`: `progman` is Lively's
//! default on the raised desktop (just under SHELLDLL_DefView), `workerw` its "WorkerW parent"
//! setting. The GPU modes use the window styles of Wallpaper Engine's video renderer and a DXGI
//! swap chain, presented to the HWND or via DirectComposition. Logs go to `desktop-lively.log`.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
#[path = "lively/flip.rs"]
mod flip;

#[cfg(target_os = "windows")]
fn main() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: process-wide DPI mode before any window exists (Lively is per-monitor aware).
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().cloned().unwrap_or_else(|| "progman".into());
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
    let rgb = args.get(2).and_then(|s| u32::from_str_radix(s, 16).ok()).unwrap_or(0xC0_20_80);
    let mut log = Vec::new();
    let render = args.get(3).map_or("gdi", String::as_str);
    win::run(&mode, secs, rgb, render, &mut log);
    win::flush(&mut log);
}

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
mod win {
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, MapWindowPoints, UpdateWindow, PAINTSTRUCT, SetBkMode,
        SetTextColor, TRANSPARENT, TextOutW,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::SHQueryUserNotificationState;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, FindWindowExW, FindWindowW,
        GW_CHILD, GW_HWNDNEXT, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetClientRect, GetSystemMetrics, GetWindow,
        GetLayeredWindowAttributes, GetWindowLongPtrW, GetWindowRect, GetWindowTextW, LWA_ALPHA,
        MSG, PM_REMOVE, PeekMessageW, RegisterClassExW,
        SM_CXSCREEN, SM_CYSCREEN, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SW_SHOWNA, SendMessageTimeoutW, SetLayeredWindowAttributes, SetParent, ShowWindow, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
        WM_PAINT, WNDCLASSEXW, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
        WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows_core::{BOOL, w};

    static PAINTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static COLOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn class(h: HWND) -> String {
        let mut b = [0u16; 64];
        // SAFETY: buffer and length.
        let n = unsafe { GetClassNameW(h, &mut b) } as usize;
        String::from_utf16_lossy(&b[..n])
    }

    fn describe(h: HWND, depth: usize, out: &mut Vec<String>) {
        let mut t = [0u16; 64];
        let mut r = RECT::default();
        // SAFETY: reads of a window's text, styles and rectangle.
        unsafe {
            let n = GetWindowTextW(h, &mut t) as usize;
            let _ = GetWindowRect(h, &mut r);
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            let mut layer = String::new();
            if ex & WS_EX_LAYERED.0 as isize != 0 {
                let (mut key, mut alpha, mut flags) = (COLORREF(0), 0, Default::default());
                if GetLayeredWindowAttributes(h, Some(&mut key), Some(&mut alpha), Some(&mut flags)).is_ok() {
                    layer = format!(" layer flags {:#x} alpha {alpha} key {:#08x}", flags.0, key.0);
                }
            }
            out.push(format!(
                "{}{:?} {} \"{}\" style {:#x} ex {ex:#x} rect {},{} {}x{}{layer}",
                "  ".repeat(depth),
                h.0,
                class(h),
                String::from_utf16_lossy(&t[..n]),
                GetWindowLongPtrW(h, GWL_STYLE),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top
            ));
            if depth < 2 {
                let mut c = GetWindow(h, GW_CHILD).ok();
                while let Some(ch) = c {
                    describe(ch, depth + 1, out);
                    c = GetWindow(ch, GW_HWNDNEXT).ok();
                }
            }
        }
    }

    struct Found {
        defview: Option<HWND>,
        workerw: Option<HWND>,
    }

    unsafe extern "system" fn each(top: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Found` of `run`, alive for the EnumWindows call.
        let f = unsafe { &mut *(data.0 as *mut Found) };
        // SAFETY: plain window lookups, as Lively does them.
        unsafe {
            if let Ok(p) = FindWindowExW(Some(top), None, w!("SHELLDLL_DefView"), None) {
                f.workerw = FindWindowExW(None, Some(top), w!("WorkerW"), None).ok();
                f.defview = Some(p);
            }
        }
        BOOL(1)
    }

    unsafe extern "system" fn proc_(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: a paint cycle of this window with a brush freed here.
        unsafe {
            if msg == WM_PAINT {
                PAINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(h, &mut ps);
                let mut rc = RECT::default();
                let _ = GetClientRect(h, &mut rc);
                let c = COLOR.load(std::sync::atomic::Ordering::Relaxed);
                let b = CreateSolidBrush(COLORREF(((c & 0xFF) << 16) | (c & 0xFF00) | (c >> 16)));
                FillRect(dc, &rc, b);
                let _ = DeleteObject(b.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0xFFFFFF));
                let text: Vec<u16> = "desktop-lively: wallpaper window behind the icons".encode_utf16().collect();
                let _ = TextOutW(dc, rc.right / 2 - 160, rc.bottom / 2, &text);
                let _ = EndPaint(h, &ps);
                return LRESULT(0);
            }
            DefWindowProcW(h, msg, wp, lp)
        }
    }

    /// Writes the lines so far beside the exe (the test may be killed before it ends).
    pub fn flush(log: &mut Vec<String>) {
        use std::io::Write;
        let path = std::env::current_exe().unwrap_or_default().with_file_name("desktop-lively.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = write!(f, "{}\r\n", log.join("\r\n"));
        }
        log.clear();
    }

    pub fn run(mode: &str, secs: u64, rgb: u32, render: &str, log: &mut Vec<String>) {
        // SAFETY: the test's own window, created, parented, pumped and destroyed here.
        unsafe {
            COLOR.store(rgb, std::sync::atomic::Ordering::Relaxed);
            let Ok(progman) = FindWindowW(w!("Progman"), None) else {
                log.push("no Progman".into());
                return;
            };
            let raised_layout = GetWindowLongPtrW(progman, GWL_EXSTYLE) & WS_EX_NOREDIRECTIONBITMAP.0 as isize != 0;
            log.push(format!("mode {mode}; Progman {:?}, WS_EX_NOREDIRECTIONBITMAP {raised_layout}", progman.0));
            log.push(format!("SHQueryUserNotificationState: {:?}", SHQueryUserNotificationState().map(|state| state.0)));
            log.push("tree before:".into());
            describe(progman, 0, log);
            let t = Instant::now();
            let mut res = 0usize;
            let sent = SendMessageTimeoutW(progman, 0x052C, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, Some(&mut res));
            log.push(format!("0x052C sent: {} in {} us", sent.0 != 0, t.elapsed().as_micros()));
            let mut f = Found { defview: None, workerw: None };
            let _ = EnumWindows(Some(each), LPARAM(&mut f as *mut _ as isize));
            log.push(format!("EnumWindows: DefView {:?}, next top-level WorkerW {:?}", f.defview.map(|h| h.0), f.workerw.map(|h| h.0)));
            if raised_layout {
                f.workerw = FindWindowExW(Some(progman), None, w!("WorkerW"), None).ok();
                log.push(format!("raised: WorkerW child of Progman {:?}", f.workerw.map(|h| h.0)));
            }
            log.push("tree after 0x052C:".into());
            describe(progman, 0, log);
            let (Some(defview), Some(workerw)) = (f.defview, f.workerw) else {
                log.push("FAIL: no DefView or WorkerW".into());
                return;
            };
            if matches!(render, "flip" | "composition") {
                let parent = if mode == "workerw" { workerw } else { progman };
                let after = (parent == progman).then_some(defview);
                if let Err(error) = run_gpu(progman, parent, after, secs, rgb, render, log) {
                    log.push(format!("FAIL: {render} probe setup or presentation: {error}"));
                }
                return;
            }

            let inst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc_),
                hInstance: inst.into(),
                lpszClassName: w!("desktop-lively"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let Ok(h) = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("desktop-lively"),
                w!("desktop-lively"),
                WS_POPUP,
                0,
                0,
                sw,
                sh,
                None,
                None,
                Some(inst.into()),
                None,
            ) else {
                log.push("FAIL: window not created".into());
                return;
            };
            // SetParent does not update WS_POPUP/WS_CHILD. Make it a child before attaching.
            SetWindowLongPtrW(h, GWL_STYLE, WS_CHILD.0 as isize);
            let parent = if mode == "workerw" {
                SetParent(h, Some(workerw)).is_ok().then_some(workerw)
            } else {
                // Lively: WS_CHILD, WS_EX_LAYERED alpha 255, SetParent(progman), then under the DefView.
                let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
                let p = SetParent(h, Some(progman)).is_ok().then_some(progman);
                let _ = SetWindowPos(h, Some(defview), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                p
            };
            let Some(parent) = parent else {
                log.push("FAIL: SetParent".into());
                let _ = DestroyWindow(h);
                return;
            };
            // Lively fills the display: screen rectangle mapped into the parent.
            let mut pts = [POINT { x: 0, y: 0 }, POINT { x: sw, y: sh }];
            MapWindowPoints(None, Some(parent), &mut pts);
            let _ = SetWindowPos(
                h,
                None,
                pts[0].x,
                pts[0].y,
                pts[1].x - pts[0].x,
                pts[1].y - pts[0].y,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            if mode != "workerw" {
                // Again once a child: the attributes set while top-level may not carry over.
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            }
            let _ = ShowWindow(h, SW_SHOWNA);
            let _ = InvalidateRect(Some(h), None, true);
            let _ = UpdateWindow(h);
            log.push(format!("window {:?} parented to {} {:?}", h.0, class(parent), parent.0));
            log.push("tree with the window:".into());
            describe(progman, 0, log);
            flush(log);
            let end = Instant::now() + Duration::from_secs(secs);
            let mut msg = MSG::default();
            while Instant::now() < end {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            let _ = DestroyWindow(h);
            log.push(format!("done, {} paints, window destroyed", PAINTS.load(std::sync::atomic::Ordering::Relaxed)));
        }
    }

    fn run_gpu(
        progman: HWND, parent: HWND, after: Option<HWND>, secs: u64, rgb: u32, render: &str,
        log: &mut Vec<String>,
    ) -> windows_core::Result<()> {
        // SAFETY: this thread owns the probe window and its renderer. The parent is discovered
        // above, and the primary screen bounds are mapped into its client coordinates.
        unsafe {
            let inst = GetModuleHandleW(None)?;
            let name = w!("desktop-lively-flip");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(flip_proc),
                hInstance: inst.into(),
                lpszClassName: name,
                ..Default::default()
            };
            if RegisterClassExW(&wc) == 0 {
                return Err(windows_core::Error::from_thread());
            }
            let (width, height) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let mut bounds = [POINT::default(), POINT { x: width, y: height }];
            MapWindowPoints(None, Some(parent), &mut bounds);
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP | WS_EX_LAYERED,
                name,
                name,
                WS_CHILD,
                bounds[0].x,
                bounds[0].y,
                width,
                height,
                Some(parent),
                None,
                Some(inst.into()),
                None,
            )?;
            let result = (|| {
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
                let flags = SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE;
                if let Some(after) = after {
                    SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, flags)?;
                }
                let _ = ShowWindow(hwnd, SW_SHOWNA);
                let renderer = crate::flip::Renderer::new(
                    hwnd, width as u32, height as u32, rgb, render == "composition",
                )?;
                log.push(format!("{render} window {:?} parented to {} {:?}", hwnd.0, class(parent), parent.0));
                describe(progman, 0, log);
                let first = renderer.present()?;
                log.push(format!("{render} present {:#010x}; expect #{rgb:06X} at {},{}", first.0, width / 2, height / 2));
                flush(log);
                let end = Instant::now() + Duration::from_secs(secs);
                let mut msg = MSG::default();
                while Instant::now() < end {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    let _ = renderer.present()?;
                    std::thread::sleep(Duration::from_millis(30));
                }
                Ok(())
            })();
            let _ = DestroyWindow(hwnd);
            result
        }
    }

    unsafe extern "system" fn flip_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // SAFETY: the flip window uses only the default window handling.
        unsafe { DefWindowProcW(h, msg, wp, lp) }
    }
}
