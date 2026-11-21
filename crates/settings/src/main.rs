#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod view;
#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    if let Err(error) = win::run() {
        win::report_error(&error.to_string());
    }
}

#[cfg(not(windows))]
fn main() {}
