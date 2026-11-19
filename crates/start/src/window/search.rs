//! Search inside Start: every key ranks apps and settings on the UI thread and draws at once
//! (timed from the key message to the frame DWM shows), files stream in from the index worker,
//! and the actions of the results.

use super::{Target, hide, launch, popup, with};
use crate::index::{Batch, Index};
use crate::mft::Mft;
use crate::scene::{Act, View};
use crate::search::{self, Kind};
use crate::sources::{self, Setting};
use crate::{apps, log, power};
use std::cell::RefCell;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
use windows::Win32::UI::WindowsAndMessaging::{GetMessageTime, PostMessageW};
use windows_core::HSTRING;

thread_local! {
    static INDEX: RefCell<Option<Index>> = const { RefCell::new(None) };
    static MFT: RefCell<Option<Mft>> = const { RefCell::new(None) };
    /// When the key of the current query was handled, for the file batches' timing.
    static KEY_AT: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

const CALCULATOR: &str = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";

/// Starts the index worker and the Settings loader.
pub fn start(hwnd: HWND, icon_px: i32) {
    INDEX.with(|i| *i.borrow_mut() = Some(Index::start(hwnd, icon_px)));
    MFT.with(|i| *i.borrow_mut() = Some(Mft::start(hwnd, icon_px)));
    let target = hwnd.0 as isize;
    let _ = std::thread::Builder::new().name("start-settings".into()).spawn(move || {
        // SAFETY: COM for this worker (indirect strings from package resources); the box goes to the UI thread.
        unsafe {
            let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
            let t = Instant::now();
            let v = sources::load_settings();
            log::line(&format!("settings: loaded in {} ms", t.elapsed().as_millis()));
            let b = Box::into_raw(Box::new(v));
            if PostMessageW(Some(HWND(target as _)), super::WM_SETTINGS, WPARAM(0), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
            if com {
                CoUninitialize();
            }
        }
    });
}

pub fn settings_loaded(v: Vec<Setting>) {
    with(|s| {
        s.settings_index = v.iter().map(|x| search::Entry::new(&x.name, &x.keywords)).collect();
        s.settings = v;
        s.warm();
    });
}

/// A typed character: the query grows and the results answer in the same frame.
pub fn ch(c: u32) {
    let Some(c) = char::from_u32(c).filter(|c| !c.is_control()) else { return };
    let label = c.to_string();
    answer(&label, |q| {
        let mut q = q.to_string();
        q.push(c);
        q
    });
}

pub fn backspace() {
    answer("backspace", |q| {
        let mut q = q.to_string();
        q.pop();
        q
    });
}

fn qpc() -> i64 {
    let mut v = 0;
    // SAFETY: writes the counter into a local.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v
}

/// The QPC time of the first vblank after `at`, when DWM composes the frame presented before it,
/// and the refresh period, from DWM's timing info.
fn next_vblank(at: i64) -> Option<(i64, i64)> {
    let mut t = DWM_TIMING_INFO { cbSize: size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    // SAFETY: fills a local struct sized above.
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut t) }.ok()?;
    let period = t.qpcRefreshPeriod as i64;
    if period <= 0 {
        return None;
    }
    let mut v = t.qpcVBlank as i64;
    if v < at {
        v += ((at - v) / period + 1) * period;
    }
    Some((v, period))
}

fn answer(key: &str, edit: impl FnOnce(&str) -> String) {
    let mut freq = 0;
    // SAFETY: writes the frequency into a local.
    let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
    let ms = |a: i64, b: i64| (b - a) as f32 * 1000.0 / freq.max(1) as f32;
    let q0 = qpc();
    // SAFETY: the time of the key message being handled and the tick now, both in ms.
    let queued = unsafe { (GetTickCount() as i32).wrapping_sub(GetMessageTime()) };
    let t0 = Instant::now();
    let Some((generation, q, ranked, apps, settings, calc)) = with(|s| {
        if !s.shown {
            return None;
        }
        let q = edit(&s.query);
        let generation = s.set_query(q.clone());
        s.search_focus = true;
        let ranked = t0.elapsed().as_secs_f32() * 1000.0;
        let _ = s.draw();
        Some((generation, q, ranked, s.found.apps.len(), s.found.settings.len(), s.found.calc.is_some()))
    })
    .flatten() else {
        return;
    };
    MFT.with(|i| {
        if let Some(i) = i.borrow().as_ref() {
            i.query(generation, &q);
        }
    });
    INDEX.with(|i| {
        if let Some(i) = i.borrow().as_ref() {
            i.query(generation, &q);
        }
    });
    KEY_AT.with(|k| *k.borrow_mut() = Some(t0));
    let qp = qpc();
    let present = ms(q0, qp);
    let vblank = next_vblank(qp);
    // SAFETY: waits for the composition that shows the frame just presented.
    let _ = unsafe { DwmFlush() };
    let flushed = t0.elapsed().as_secs_f32() * 1000.0;
    let queued = queued.max(0) as f32;
    let (frame, refresh) = vblank.map_or((flushed, 0.0), |(v, p)| (ms(q0, v), ms(0, p)));
    let total = queued + frame;
    log::line(&format!(
        "search key {key} \"{q}\": queue {queued} ms, ranked {ranked:.2} ms, presented {present:.1} ms, shown at the vblank {frame:.1} ms (refresh {refresh:.1} ms), DwmFlush back {flushed:.1} ms, key to frame {total:.1} ms{}; apps {apps}, settings {settings}{}",
        if total <= 16.0 { "" } else { " (over 16)" },
        if calc { ", calculator" } else { "" }
    ));
}

/// A batch of files from the index worker.
pub fn files(b: Batch) {
    let after = KEY_AT.with(|k| k.borrow().map(|t| t.elapsed().as_secs_f32() * 1000.0)).unwrap_or(0.0);
    let n = b.hits.len();
    let t = Instant::now();
    let shown = with(|s| {
        let entries = b
            .hits
            .into_iter()
            .map(|h| crate::scene::FileEntry {
                icon: h.icon.as_ref().and_then(|i| s.bitmap(i)),
                name: h.name,
                path: h.path,
                type_text: h.type_text,
                folder: h.folder,
                rank: h.rank,
            })
            .collect();
        let ok = s.add_files(b.generation, entries, b.done);
        if ok {
            let _ = s.draw();
        }
        ok.then(|| s.query.clone())
    })
    .flatten();
    if let Some(q) = shown {
        if n > 0 || b.done {
            log::line(&format!(
                "search files \"{q}\": +{n} from {}{} at {after:.1} ms after the key (source {:.1} ms), drawn in {:.1} ms",
                b.source,
                if b.done { ", done" } else { "" },
                b.ms,
                t.elapsed().as_secs_f32() * 1000.0
            ));
        }
    }
}

/// What the selected result and one of its actions point at.
enum Do {
    App(String, Act),
    Setting(String),
    Path(String, bool, Act),
    Calc(String, Act),
}

/// Runs action `i` of the selected result.
pub fn run_action(i: usize) {
    let Some((d, name, hwnd)) = with(|s| {
        let h = s.selected()?;
        let act = *s.actions_of(h).get(i)?;
        let d = match h.kind {
            Kind::App => Do::App(s.apps[h.index].aumid.clone(), act),
            Kind::Setting => Do::Setting(s.settings[h.index].link.clone()),
            Kind::File | Kind::Folder => {
                let f = &s.found.files[h.index];
                Do::Path(f.path.clone(), f.folder, act)
            }
            Kind::Calc => Do::Calc(s.found.calc.clone()?.1, act),
        };
        let name = match h.kind {
            Kind::App => s.apps[h.index].name.clone(),
            Kind::Setting => s.settings[h.index].name.clone(),
            Kind::File | Kind::Folder => s.found.files[h.index].name.clone(),
            Kind::Calc => "calculator".into(),
        };
        Some((d, name, s.hwnd))
    })
    .flatten() else {
        return;
    };
    match d {
        Do::App(aumid, Act::Open) => launch(Target::App(aumid), "search"),
        Do::App(aumid, Act::RunAs) => {
            log::line(&format!("search action run as administrator: {name}"));
            background("run as admin", move || sources::shell_execute(&format!("shell:AppsFolder\\{aumid}"), Some("runas"), None));
            hide(false, "launched");
        }
        Do::App(aumid, Act::Location) => {
            folder_action(&name, move || sources::shell_execute(&format!("shell:AppsFolder\\{aumid}"), Some("openfilelocation"), None));
        }
        Do::App(aumid, a @ (Act::Pin | Act::Unpin)) => {
            with(|s| {
                if a == Act::Pin {
                    s.pins.push(aumid.clone());
                } else {
                    s.pins.retain(|p| !p.eq_ignore_ascii_case(&aumid));
                }
                apps::save_pins(&s.pins);
                s.dirty = true;
                let _ = s.draw();
            });
            log::line(&format!("search action {}: {aumid}", if a == Act::Pin { "pin" } else { "unpin" }));
        }
        Do::Setting(link) => {
            log::line(&format!("search action open setting {name}: {link}"));
            let l = link.clone();
            background("setting", move || sources::shell_execute(&l, None, None));
            hide(false, "launched");
        }
        Do::Path(path, false, Act::Open) => launch(Target::File(path), "search"),
        Do::Path(path, true, Act::Open) => {
            // A folder opens in File Explorer: never start explorer.exe from a test session.
            folder_action(&name, move || sources::shell_execute(&path, None, None));
        }
        Do::Path(path, _, Act::Location) => folder_action(&name, move || open_location(&path)),
        Do::Path(path, _, _) => {
            let ok = sources::copy_text(hwnd, &path);
            log::line(&format!("search action copy full path: {path}: {}", if ok { "copied" } else { "clipboard busy" }));
            hide(true, "copied");
        }
        Do::Calc(v, Act::Copy) => {
            let ok = sources::copy_text(hwnd, &v);
            log::line(&format!("search action copy calculator result {v}: {}", if ok { "copied" } else { "clipboard busy" }));
            hide(true, "copied");
        }
        Do::Calc(_, _) => launch(Target::App(CALCULATOR.into()), "search calculator"),
        Do::App(..) => {}
    }
}

/// Runs a shell call on a worker thread (COM up), logging a failure.
fn background(what: &'static str, f: impl FnOnce() -> Result<(), String> + Send + 'static) {
    let _ = std::thread::Builder::new().name("start-action".into()).spawn(move || {
        // SAFETY: COM for this worker thread, balanced below.
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let t = Instant::now();
        match f() {
            Ok(()) => log::line(&format!("search action {what}: done in {} ms", t.elapsed().as_millis())),
            Err(e) => log::line(&format!("search action {what}: failed: {e}")),
        }
        if com {
            // SAFETY: balances the call above.
            unsafe { CoUninitialize() };
        }
    });
}

/// Opens a folder window, which is File Explorer until explorust's own: logged only in the dev exe.
fn folder_action(name: &str, f: impl FnOnce() -> Result<(), String> + Send + 'static) {
    if power::is_dry() {
        log::line(&format!("search action folder for {name}: dry run, a folder window would start explorer.exe"));
        hide(true, "folder dry run");
        return;
    }
    background("folder", f);
    hide(false, "launched");
}

fn open_location(path: &str) -> Result<(), String> {
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: parses the path, opens its folder with it selected, frees the ID list.
    unsafe {
        SHParseDisplayName(&HSTRING::from(path), None, &mut pidl, 0, None).map_err(|e| format!("parse: {e}"))?;
        let r = SHOpenFolderAndSelectItems(pidl, None, 0).map_err(|e| format!("SHOpenFolderAndSelectItems: {e}"));
        CoTaskMemFree(Some(pidl as _));
        r
    }
}

/// The right-click menu of result row `i`: its actions.
pub fn context(i: usize, x: f32, y: f32) {
    let Some(items) = with(|s| {
        if s.view != View::Search {
            return None;
        }
        s.found.sel = Some(i);
        s.dirty = true;
        let _ = s.draw();
        let acts = s.actions();
        Some(acts.into_iter().enumerate().map(|(k, a)| (100 + k as u32, s.act_label(a))).collect::<Vec<_>>())
    })
    .flatten() else {
        return;
    };
    let id = popup(x, y, &items, false);
    if id >= 100 {
        run_action((id - 100) as usize);
    }
}
