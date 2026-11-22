//! The Windows side: the bar window and its message loop.

mod actions;
mod appbars;
mod app_resolver;
mod bar;
mod context;
mod jumplist;
mod log;
mod menu;
mod pin_worker;
mod popup;
mod scene;
mod side;
mod status;
mod settings;
mod taskband;
mod task_lookup;
mod tasks;
mod thumbs;
mod trayhost;
mod workarea;
mod winx;

pub use winx::set_dry_power;

pub fn run() {
    bar::run();
}
