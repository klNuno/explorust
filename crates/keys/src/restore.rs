//! Keep restoration order, then focus the highest window that actually restored.

use std::time::Instant;

fn wait_for(deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    false
}

fn batch<T: Copy, R>(
    windows: impl IntoIterator<Item = T>,
    mut restore: impl FnMut(T) -> bool,
    focus: impl FnOnce(T) -> R,
) -> Option<R> {
    let mut top = None;
    for window in windows {
        if restore(window) {
            top = Some(window);
        }
    }
    top.map(focus)
}

#[cfg(target_os = "windows")]
mod win {
    use super::{batch, wait_for};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOTOWNER, GW_HWNDPREV, GWL_EXSTYLE, GetAncestor, GetWindow, GetWindowLongW, HWND_TOP,
        IsIconic, IsWindow, IsWindowVisible, SMTO_ABORTIFHUNG, SMTO_BLOCK, SW_SHOWNOACTIVATE,
        SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        SendMessageTimeoutW, SetForegroundWindow, SetWindowPos, ShowWindowAsync, WM_NULL,
        WS_EX_TOPMOST,
    };

    const BUDGET: Duration = Duration::from_millis(100);

    fn responsive(h: HWND, deadline: Instant) -> bool {
        let remaining = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        if remaining == 0 {
            return false;
        }
        // SAFETY: WM_NULL has no parameters or side effects and the wait has a fixed deadline.
        unsafe {
            SendMessageTimeoutW(
                h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG | SMTO_BLOCK, remaining, None,
            ).0 != 0
        }
    }

    fn at_top(h: HWND, deadline: Instant) -> bool {
        // SAFETY: these read the window manager's ordering and styles without sending messages.
        unsafe {
            let topmost = GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0;
            let mut current = h;
            while let Ok(above) = GetWindow(current, GW_HWNDPREV) {
                if Instant::now() >= deadline {
                    return false;
                }
                if IsWindowVisible(above).as_bool() && !IsIconic(above).as_bool()
                    && GetWindowLongW(above, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 == topmost
                    && GetAncestor(above, GA_ROOTOWNER) != h
                {
                    return false;
                }
                current = above;
            }
            IsWindow(Some(h)).as_bool()
        }
    }

    pub(crate) fn restore(h: HWND) -> bool {
        let deadline = Instant::now() + BUDGET;
        // SAFETY: the requests target windows on separate UI queues. Use only asynchronous
        // mutations: a prompt WM_NULL cannot guarantee the target stays responsive for them.
        unsafe {
            if !responsive(h, deadline) || !ShowWindowAsync(h, SW_SHOWNOACTIVATE).as_bool()
                || !wait_for(deadline, || IsWindow(Some(h)).as_bool() && !IsIconic(h).as_bool())
                || !responsive(h, deadline)
            {
                return false;
            }
            if at_top(h, deadline) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
            SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, flags).is_ok()
                && wait_for(deadline, || at_top(h, deadline))
                && responsive(h, deadline)
        }
    }

    pub fn restore_desktop(
        windows: impl IntoIterator<Item = isize>,
        grant_foreground: bool,
        mut report: impl FnMut(isize, bool),
    ) -> Option<(isize, bool)> {
        batch(windows, |id| {
            let h = HWND(id as _);
            let ok = super::super::activate::show_desktop_target(h, false).is_ok() && restore(h);
            report(id, ok);
            ok
        }, |id| {
            let h = HWND(id as _);
            if grant_foreground {
                super::super::activate::grant_foreground(h, h);
            }
            // Do not attach input queues: foreground activation is asynchronous across queues.
            // SAFETY: a foreground request on the last successfully restored window.
            (id, unsafe { SetForegroundWindow(h) }.as_bool())
        })
    }
}

#[cfg(target_os = "windows")]
pub use win::restore_desktop;
#[cfg(target_os = "windows")]
pub(crate) use win::restore;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::time::Duration;

    #[test]
    fn restore_and_raise_stop_waiting_at_the_target_deadline() {
        let mut checks = 0;
        assert!(!wait_for(Instant::now() - Duration::from_millis(1), || {
            checks += 1;
            checks == 3
        }));
    }

    #[test]
    fn the_batch_skips_stalled_windows_and_activates_the_highest_responsive_one() {
        let order = RefCell::new(Vec::new());
        batch([1, 2, 3, 4], |id| {
            order.borrow_mut().push(("restore", id));
            id == 1 || id == 3
        }, |id| order.borrow_mut().push(("focus", id)));
        assert_eq!(*order.borrow(), [
            ("restore", 1), ("restore", 2), ("restore", 3), ("restore", 4), ("focus", 3),
        ]);
    }
}
