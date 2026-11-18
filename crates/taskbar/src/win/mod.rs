//! The Windows side: the bar window and its message loop.

mod actions;
mod bar;
mod log;
mod scene;
mod tasks;
mod workarea;

pub fn run() {
    bar::run();
}
