//! Settings' system backdrop: Settings (WinUI 2's MicaController) gives its XAML window a Mica
//! brush through `ICompositionSupportsSystemBackdrop`; that brush, or its solid fallback when Mica
//! is off, is the opaque layer under the whole page. The TAP replaces it on the UI thread, from the
//! `Window` element the tree watch reports, and sets it again whenever Settings puts its own back
//! (the controller swaps Mica for its inactive color when the window loses focus).

use super::abi::{IXamlDiagnostics, InstanceHandle};
use super::report;
use super::rules::argb;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Foundation::TypedEventHandler;
use windows::UI::Color;
use windows::UI::Composition::{CompositionBrush, Compositor, ICompositionSupportsSystemBackdrop};
use windows::UI::Core::{CoreDispatcherPriority, CoreWindow, DispatchedHandler, WindowActivatedEventArgs};
use windows_core::{IInspectable, Interface};

/// What [`restore`] needs to give the window its own backdrop back: the old brush and the
/// activation handler to revoke. Used on the UI thread only.
pub struct Kept {
    target: ICompositionSupportsSystemBackdrop,
    old: Option<CompositionBrush>,
    window: Option<CoreWindow>,
    token: i64,
    live: Arc<AtomicBool>,
}
impl Kept {
    pub fn pause(&self) {
        self.live.store(false, Ordering::SeqCst);
    }
}
// SAFETY: made and used on Settings' UI thread (the tree callback and that window's dispatcher);
// other threads only move it.
unsafe impl Send for Kept {}

/// Replaces the backdrop of the window behind handle `h` with `what` (`none`, `host`, `#AARRGGBB`);
/// returns "old -> new" for the log and what restores it.
pub fn apply(diag: &IXamlDiagnostics, h: InstanceHandle, what: &str) -> Result<(String, Kept), String> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: an out pointer; the returned reference is owned by `obj`.
    let hr = unsafe { diag.GetIInspectableFromHandle(h, &mut raw) };
    if hr.is_err() || raw.is_null() {
        return Err(format!("GetIInspectableFromHandle {:#010x}", hr.0));
    }
    // SAFETY: an owned IInspectable reference.
    let obj = unsafe { IInspectable::from_raw(raw) };
    let target: ICompositionSupportsSystemBackdrop = obj.cast().map_err(|e| format!("no system backdrop: {e}"))?;
    let old = target.SystemBackdrop().ok();
    let old_name = old.as_ref().map_or("null".to_string(), name);
    let new: Option<CompositionBrush> = match what {
        "none" => None,
        _ => {
            let c: Compositor = old
                .as_ref()
                .ok_or("no compositor: the window has no backdrop yet")?
                .Compositor()
                .map_err(|e| e.to_string())?;
            Some(if what == "host" {
                c.CreateHostBackdropBrush().and_then(|b| b.cast()).map_err(|e| e.to_string())?
            } else {
                let [a, r, g, b] = argb(what).ok_or_else(|| format!("bad backdrop {what}"))?;
                c.CreateColorBrushWithColor(Color { A: a, R: r, G: g, B: b })
                    .and_then(|b| b.cast())
                    .map_err(|e| e.to_string())?
            })
        }
    };
    target.SetSystemBackdrop(new.as_ref()).map_err(|e| format!("SetSystemBackdrop: {e}"))?;
    let live = Arc::new(AtomicBool::new(true));
    let (window, token, kept) = match keep(target.clone(), new, live.clone()) {
        Ok((w, t)) => (Some(w), t, "kept on activation"),
        Err(e) => {
            report::line(format!("system backdrop not kept: {e}"));
            (None, 0, "not kept")
        }
    };
    Ok((
        format!("{old_name} -> {what}, {kept}"),
        Kept {
            target,
            old,
            window,
            token,
            live,
        },
    ))
}

/// On the UI thread: revokes the activation handler and puts Settings' brush back. The caller
/// retains this record if either operation fails.
pub fn restore(k: &mut Kept) -> windows_core::Result<String> {
    k.pause();
    if let Some(w) = &k.window {
        w.RemoveActivated(k.token)?;
        k.window = None;
    }
    k.target.SetSystemBackdrop(k.old.as_ref())?;
    Ok(format!("system backdrop {} again", k.old.as_ref().map_or("null".into(), name)))
}

/// The window's backdrop and the brush we want there, used on the UI thread only.
#[derive(Clone)]
struct Want(ICompositionSupportsSystemBackdrop, Option<CompositionBrush>, Arc<AtomicBool>);
// SAFETY: the handlers that carry it run on the UI thread that made it (CoreWindow events and
// that window's dispatcher).
unsafe impl Send for Want {}
unsafe impl Sync for Want {}

/// After each activation change, once Settings' own handlers ran (a low priority dispatch), puts
/// our brush back if the backdrop is no longer it.
/// Returns the window and the handler's token. A dispatch queued before [`restore`] does nothing
/// once `live` is false.
fn keep(target: ICompositionSupportsSystemBackdrop, brush: Option<CompositionBrush>, live: Arc<AtomicBool>) -> windows_core::Result<(CoreWindow, i64)> {
    let window = CoreWindow::GetForCurrentThread()?;
    let dispatcher = window.Dispatcher()?;
    let want = Want(target, brush, live);
    let token = window.Activated(&TypedEventHandler::<CoreWindow, WindowActivatedEventArgs>::new(move |_, _| {
        let want = want.clone();
        dispatcher.RunAsync(
            CoreDispatcherPriority::Low,
            &DispatchedHandler::new(move || {
                // The whole struct is moved in (Send), not its fields one by one.
                let want = &want;
                if !want.2.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let now = want.0.SystemBackdrop().ok();
                if now.as_ref().map(Interface::as_raw) != want.1.as_ref().map(Interface::as_raw) {
                    want.0.SetSystemBackdrop(want.1.as_ref())?;
                    report::line(format!(
                        "system backdrop: Settings put {} back, ours set again",
                        now.as_ref().map_or("null".into(), name)
                    ));
                }
                Ok(())
            }),
        )?;
        Ok(())
    }))?;
    Ok((window, token))
}

fn name(b: &CompositionBrush) -> String {
    b.cast::<IInspectable>()
        .and_then(|i| i.GetRuntimeClassName())
        .map_or("?".into(), |n| n.to_string())
}
