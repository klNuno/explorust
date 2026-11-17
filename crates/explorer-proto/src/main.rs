//! File Explorer maquette on the D2D + DirectComposition stack: the Downloads folder from
//! real shell data, drawn 1:1 with his Explorer (MicaBar theme).
//! Usage: explorer-proto [--x N] [--y N] [--w N] [--h N] [--secs N] [--select I] [--hover I]

#![windows_subsystem = "windows"]

mod chrome;
mod groups;
mod layout;
mod list;
mod paint;
mod pane;
mod scene;
mod shell;
mod window;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let num = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<i64>().ok());
    let opts = window::Opts {
        x: num("--x").unwrap_or(40) as i32,
        y: num("--y").unwrap_or(160) as i32,
        w: num("--w").unwrap_or(1240) as i32,
        h: num("--h").unwrap_or(800) as i32,
        secs: num("--secs").unwrap_or(8).clamp(1, 10) as u32,
        select: num("--select").filter(|v| *v >= 0).map(|v| v as usize),
        hover: num("--hover").filter(|v| *v >= 0).map(|v| v as usize),
    };
    if let Err(e) = window::run(opts) {
        shelldata::bench::log(&format!("E error {e:?}"));
    }
}
