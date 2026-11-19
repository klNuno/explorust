//! The Windows side: the bar window and its message loop.

mod actions;
mod appbars;
mod bar;
mod log;
mod popup;
mod scene;
mod side;
mod status;
mod tasks;
mod trayhost;
mod workarea;

pub fn run() {
    bar::run();
}
