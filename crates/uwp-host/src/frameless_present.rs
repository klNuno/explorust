//! explorust: presentation readiness for frameless shell-experience CoreWindows.
//!
//! ShellExperienceHost's views (the notification center, the clock's calendar, toasts) are
//! top-level CoreWindows with no ApplicationFrameWindow. twinui's `UwpWindowWrapper` for such a
//! view waits on readiness flags 1 (visibility) and 2 (layout completion) that, under explorer,
//! clear through HSHELL 0x24/0x26 on twinui's shell hook sinks. When those never arrive the view
//! is never reported shown. This does for those windows what `auto_present` does for app frames:
//! the wrapper keyed by the CoreWindow gets VisibilityChanged(1, 1) and
//! HandlePresentationReadinessChange(0x26), only while it waits.

use std::{
    collections::HashMap,
    ffi::c_void,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetParent, GetWindowThreadProcessId, IsWindowVisible,
        },
    },
    core::BOOL,
};

/// App ids whose CoreWindows are shell views without a frame.
fn is_shell_experience(app_id: &str) -> bool {
    app_id.starts_with("Microsoft.Windows.ShellExperienceHost_")
}

/// Top-level CoreWindows (no parent) with their process ids.
fn top_level_cores() -> Vec<(usize, u32)> {
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let mut name = [0u16; 64];
        // SAFETY: Windows validates the HWND; the buffer is writable.
        let length = unsafe { GetClassNameW(hwnd, &mut name) }.max(0) as usize;
        if String::from_utf16_lossy(&name[..length]) == "Windows.UI.Core.CoreWindow"
            && unsafe { GetParent(hwnd) }.is_err()
        {
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            // SAFETY: EnumWindows calls back synchronously with this Vec.
            unsafe { &mut *(data.0 as *mut Vec<(usize, u32)>) }.push((hwnd.0 as usize, pid));
        }
        BOOL(1)
    }
    let mut result = Vec::new();
    // SAFETY: the Vec outlives the synchronous enumeration.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut result as *mut _ as isize)) };
    result
}

#[derive(Default)]
struct Core {
    next_attempt: Option<Instant>,
    last: String,
}

#[derive(Default)]
pub(crate) struct FramelessPresent {
    cores: HashMap<usize, Core>,
    app_ids: HashMap<u32, Option<String>>,
}

impl FramelessPresent {
    pub(crate) fn tick(&mut self) {
        let now = Instant::now();
        let live = top_level_cores();
        self.cores.retain(|hwnd, _| live.iter().any(|(h, _)| h == hwnd));
        self.app_ids.retain(|pid, _| live.iter().any(|(_, p)| p == pid));
        for (hwnd, pid) in live {
            let app_id = self
                .app_ids
                .entry(pid)
                .or_insert_with(|| crate::auto_present::process_app_id(pid))
                .clone();
            let Some(app_id) = app_id.filter(|id| is_shell_experience(id)) else {
                continue;
            };
            let core = self.cores.entry(hwnd).or_default();
            if core.next_attempt.is_some_and(|at| now < at) {
                continue;
            }
            // SAFETY: Windows validates the HWND.
            let visible = unsafe { IsWindowVisible(HWND(hwnd as *mut c_void)) }.as_bool();
            let state = match crate::wrapper_inspect::find_wrapper(hwnd) {
                Err(error) => {
                    // No wrapper yet (the view was never shown): a full scan, so back off.
                    core.next_attempt = Some(now + Duration::from_secs(5));
                    format!("app={app_id:?} core={hwnd:#x} visible={visible} wrapper=none ({error})")
                }
                Ok(wrapper) if wrapper.client != hwnd => {
                    core.next_attempt = Some(now + Duration::from_secs(5));
                    format!(
                        "app={app_id:?} core={hwnd:#x} visible={visible} client={:#x} differs",
                        wrapper.client
                    )
                }
                Ok(wrapper) => {
                    core.next_attempt = None;
                    let flags = wrapper.wait_flags;
                    let mut calls = String::new();
                    if flags & 1 != 0 {
                        let r = crate::presentation_callbacks::probe_window_visibility(
                            wrapper.interface,
                            hwnd,
                        );
                        calls.push_str(&format!(" visibility={r:?}"));
                    }
                    if flags & 2 != 0 {
                        let r =
                            crate::presentation_callbacks::probe_window_layout(wrapper.interface, hwnd);
                        calls.push_str(&format!(" layout={r:?}"));
                    }
                    if !calls.is_empty() {
                        hlog!(
                            "phase=frameless-present app={app_id:?} core={hwnd:#x} visible={visible} wait_flags={flags:#x}{calls}"
                        );
                    }
                    format!("app={app_id:?} core={hwnd:#x} visible={visible} wait_flags={flags:#x}")
                }
            };
            if state != core.last {
                hlog!("phase=frameless-state {state}");
                core.last = state;
            }
        }
    }
}
