//! Keep wallpaper input intact while giving the desktop its blank-area right click.

use super::folder_view;
use crate::click_activation::ClickActivation;
use std::cell::{Cell, OnceCell};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent};
use windows::Win32::UI::Controls::{LVIS_SELECTED, LVITEMW, LVM_SETITEMSTATE};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const MSG_RIGHT: u32 = WM_APP + 0x25;

#[derive(Clone, Copy)]
struct Target {
    progman: HWND,
    view: HWND,
    worker: HWND,
}

struct Observer {
    target: Target,
    activation: Arc<ClickActivation>,
    down_on_wallpaper: Cell<Option<POINT>>,
}

thread_local! {
    static OBSERVER: OnceCell<Observer> = const { OnceCell::new() };
}

pub struct Hook {
    stop: Arc<OwnedHandle>,
    activation: Arc<ClickActivation>,
}

impl Hook {
    pub fn new(progman: HWND, view: HWND, worker: HWND) -> Result<Self, String> {
        // SAFETY: the event is unnamed and ownership of its handle is transferred once.
        let event = unsafe { CreateEventW(None, true, false, None) }.map_err(|e| e.to_string())?;
        let stop = Arc::new(unsafe { OwnedHandle::from_raw_handle(event.0) });
        let shutdown = stop.clone();
        let activation = Arc::new(ClickActivation::default());
        let hook_activation = activation.clone();
        let (progman, view, worker) = (progman.0 as usize, view.0 as usize, worker.0 as usize);
        std::thread::Builder::new()
            .name("wallpaper-input".into())
            .spawn(move || {
                let target = Target {
                    progman: HWND(progman as _),
                    view: HWND(view as _),
                    worker: HWND(worker as _),
                };
                let observer = Observer {
                    target,
                    activation: hook_activation,
                    down_on_wallpaper: Cell::new(None),
                };
                if let Err(e) = observe(observer, HANDLE(shutdown.as_raw_handle())) {
                    crate::log::line(&format!("wallpaper input observation: {e}"));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { stop, activation })
    }

    pub fn right_click_activation(&self, lp: LPARAM) -> Option<LRESULT> {
        let mouse_message = (lp.0 >> 16) as u16 as u32;
        // Progman can receive click activation after the observer has opened the menu.
        // The pointer can already be over the popup, so use the original hit from the hook.
        if matches!(mouse_message, WM_RBUTTONDOWN | WM_NCRBUTTONDOWN)
            && self
                .activation
                .suppresses_activation(unsafe { GetTickCount64() })
        {
            return Some(LRESULT(MA_NOACTIVATE as isize));
        }
        None
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // The observer owns the hook. The desktop never waits on the hook chain.
        let _ = unsafe { SetEvent(HANDLE(self.stop.as_raw_handle())) };
    }
}

fn observe(observer: Observer, stop: HANDLE) -> windows_core::Result<()> {
    // SAFETY: these calls only set up this thread and pump its message queue. The low-level
    // callback runs when the owning thread receives the system's message.
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OBSERVER.with(|slot| {
            let _ = slot.set(observer);
        });
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let hook = SetWindowsHookExW(
            WH_MOUSE_LL,
            Some(mouse),
            Some(GetModuleHandleW(None)?.into()),
            0,
        )?;
        #[cfg(test)]
        tests::mark_observing(true);
        let handles = [stop];
        loop {
            let ready = MsgWaitForMultipleObjectsEx(
                Some(&handles),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            );
            if ready.0 != WAIT_OBJECT_0.0 + 1 {
                break;
            }
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE);
        }
        #[cfg(test)]
        tests::mark_observing(false);
        UnhookWindowsHookEx(hook)
    }
}

unsafe extern "system" fn mouse(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let msg = wp.0 as u32;
    if code == HC_ACTION as i32 && matches!(msg, WM_RBUTTONDOWN | WM_RBUTTONUP) {
        OBSERVER.with(|observer| {
            let Some(observer) = observer.get() else {
                return;
            };
            let target = observer.target;
            // SAFETY: the low-level hook receives MSLLHOOKSTRUCT in screen coordinates. No COM,
            // send to a foreign thread, or menu operations are performed in the hook.
            let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
            if msg == WM_RBUTTONDOWN {
                let down = on_wallpaper(target, pt).then_some(pt);
                observer.down_on_wallpaper.set(down);
                observer
                    .activation
                    .record_down(down.is_some(), unsafe { GetTickCount64() });
            } else if observer.down_on_wallpaper.take().is_some() && on_wallpaper(target, pt) {
                let _ =
                    unsafe { PostMessageW(Some(target.progman), MSG_RIGHT, WPARAM(0), pack(pt)) };
            }
        });
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

fn on_wallpaper(target: Target, pt: POINT) -> bool {
    unsafe {
        // WindowFromPoint only sends WM_NCHITTEST to windows owned by its calling
        // thread. The observer owns none, so it never enters a wallpaper or desktop handler.
        let hit = WindowFromPoint(pt);
        // WorkerW itself already forwards mouse messages. Wallpaper apps either parent their
        // window to WorkerW or place a sibling behind DefView directly under Progman.
        hit != target.worker
            && !hit.is_invalid()
            && (IsChild(target.worker, hit).as_bool()
                || (GetAncestor(hit, GA_ROOT) == target.progman
                    && hit != target.progman
                    && hit != target.view
                    && !IsChild(target.view, hit).as_bool()))
    }
}

pub fn right_click(progman: HWND, view: HWND, worker: HWND, lp: LPARAM) {
    if !on_wallpaper(
        Target {
            progman,
            view,
            worker,
        },
        unpack(lp),
    ) {
        return;
    }
    background_menu(view, folder_view(view), lp);
}

pub(super) fn background_menu(view: HWND, list: Option<HWND>, lp: LPARAM) {
    let state = LVITEMW {
        stateMask: LVIS_SELECTED,
        ..Default::default()
    };
    // A synthetic button-down can start the list view's mouse tracking after the physical
    // button-up has already gone to the wallpaper. Open its normal context menu directly.
    unsafe {
        if let Some(list) = list {
            SendMessageW(
                list,
                LVM_SETITEMSTATE,
                Some(WPARAM(usize::MAX)),
                Some(LPARAM(&state as *const _ as isize)),
            );
        }
        let target = list.unwrap_or(view);
        let _ = PostMessageW(Some(target), WM_CONTEXTMENU, WPARAM(target.0 as usize), lp);
    }
}

pub fn unpack(lp: LPARAM) -> POINT {
    POINT {
        x: lp.0 as i16 as i32,
        y: (lp.0 >> 16) as i16 as i32,
    }
}

pub fn pack(pt: POINT) -> LPARAM {
    LPARAM((pt.y as u16 as isize) << 16 | pt.x as u16 as isize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
        SendInput,
    };
    use windows_core::w;

    static OBSERVING: AtomicBool = AtomicBool::new(false);

    pub(super) fn mark_observing(active: bool) {
        OBSERVING.store(active, Ordering::Release);
    }

    #[test]
    fn preserves_negative_desktop_coordinates() {
        for pt in [POINT { x: -1920, y: 675 }, POINT { x: 1024, y: -720 }] {
            assert_eq!(unpack(pack(pt)), pt);
        }
    }

    #[test]
    #[ignore = "requires an unlocked Windows desktop and moves the pointer"]
    fn unrelated_window_receives_click_while_desktop_is_stalled() {
        static CLICKED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn cross_thread(
            hwnd: HWND,
            msg: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            if msg == WM_LBUTTONUP {
                CLICKED.store(true, Ordering::Relaxed);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        let (ready, waiting) = std::sync::mpsc::sync_channel(0);
        let (release, stall) = std::sync::mpsc::sync_channel(0);
        let desktop = std::thread::spawn(move || {
            // This owner deliberately stops pumping after it creates the observer.
            let _hook = Hook::new(HWND::default(), HWND::default(), HWND::default()).unwrap();
            let started = Instant::now();
            while !OBSERVING.load(Ordering::Acquire) {
                assert!(
                    started.elapsed() < Duration::from_secs(1),
                    "mouse observer did not install"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            ready.send(()).unwrap();
            let _ = stall.recv_timeout(Duration::from_secs(3));
        });

        // SAFETY: the test owns this separate window and all referenced buffers.
        let latency = unsafe {
            let name = w!("explorust-wallpaper-input-fixture");
            let instance = GetModuleHandleW(None).unwrap().into();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(cross_thread),
                hInstance: instance,
                lpszClassName: name,
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                name,
                None,
                WS_POPUP,
                120,
                120,
                160,
                100,
                None,
                None,
                Some(instance),
                None,
            )
            .unwrap();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let mut original = POINT::default();
            GetCursorPos(&mut original).unwrap();
            waiting.recv_timeout(Duration::from_secs(2)).unwrap();
            let since = Instant::now();
            SetCursorPos(160, 160).unwrap();
            let inputs = [MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP].map(|flags| INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            });
            let injected = SendInput(&inputs, size_of::<INPUT>() as i32);
            let mut msg = MSG::default();
            while since.elapsed() < Duration::from_secs(2) && !CLICKED.load(Ordering::Relaxed) {
                if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let elapsed = since.elapsed();
            let _ = release.send(());
            let _ = SetCursorPos(original.x, original.y);
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(name, Some(instance));
            assert_eq!(injected, 2);
            elapsed
        };
        desktop.join().unwrap();
        assert!(
            CLICKED.load(Ordering::Relaxed),
            "unrelated window did not receive click"
        );
        println!(
            "Unrelated window received the click in {} ms",
            latency.as_millis()
        );
        assert!(
            latency < Duration::from_millis(250),
            "desktop delayed input"
        );
    }
}
