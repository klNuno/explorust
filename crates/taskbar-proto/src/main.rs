//! Candidate A: the taskbar on Direct2D + DirectComposition + DirectWrite over a DWM blur.
//! Usage: taskbar-proto [--x N] [--y N] [--w N] [--secs N] [--bench]

#![windows_subsystem = "windows"]

mod scene;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |k: &str, d: i32| {
        args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d)
    };
    let bench = args.iter().any(|a| a == "--bench");
    let opts = window::Opts {
        x: num("--x", 0),
        y: num("--y", 1344),
        w: num("--w", 2560),
        secs: num("--secs", if bench { 45 } else { 8 }) as u32,
        bench,
    };
    if let Err(e) = window::run(opts) {
        shelldata::bench::log(&format!("A error {e:?}"));
    }
}
