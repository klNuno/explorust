//! Candidate B: the same taskbar in egui (eframe, OpenGL), same layout and data as A.
//! Usage: taskbar-egui [--x N] [--y N] [--w N] [--secs N] [--bench]

#![windows_subsystem = "windows"]

mod bar;

use eframe::egui;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |k: &str, d: i32| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let bench = args.iter().any(|a| a == "--bench");
    let (x, y, w) = (num("--x", 0), num("--y", 1344), num("--w", 2560));
    let secs = num("--secs", if bench { 45 } else { 8 }) as u64;
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title("shellac taskbar B")
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_taskbar(false)
        .with_active(false)
        .with_always_on_top()
        .with_position([x as f32, y as f32])
        .with_inner_size([w as f32, shelldata::layout::BAR_H]);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let r = eframe::run_native(
        "shellac taskbar B",
        options,
        Box::new(move |cc| Ok(Box::new(bar::Bar::new(cc, w as f32, secs, bench)))),
    );
    if let Err(e) = r {
        shelldata::bench::log(&format!("B error {e:?}"));
    }
}
