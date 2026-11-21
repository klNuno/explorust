//! explorust's File Explorer windows, the folder handler.
//!
//! Windows' own folder view is hosted in our window through `IExplorerBrowser`
//! (CLSID_ExplorerBrowser) with its frames on, so the items view (columns, icons, thumbnails,
//! the context menu with third-party handlers, rename, drag and drop, ZIP folders) and the
//! navigation pane (an `INameSpaceTreeControl` the browser hosts) come from Windows. The chrome
//! above it (tab strip, back, forward, up, refresh, the breadcrumb and its edit mode, the search
//! box, the command bar) is drawn with Direct2D on a DirectComposition swap chain over Mica.
//!
//! `bus::Command::FilesOpen` (Win+E) opens a window on Home; `explorust-open.exe <path>` hands a
//! path over with `WM_COPYDATA` (`handover`). A hidden window is kept ready so a new one shows in
//! one frame.
//!
//! Contract with the shell (`crates/shell/src/components.rs`): `run()` is called on a thread of
//! its own, creates its windows (its command window has the class `bus::class_of(Owner::Files)`),
//! runs its message loop and returns on `bus::Command::Shutdown` or `WM_QUIT`.

pub mod edit;
pub mod handler;
pub mod handover;
pub mod history;
pub mod layout;
mod scope;
pub mod text;
pub mod uri;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "windows")]
pub use win::dark_mode_for_process;

/// Opens an initial folder in isolation when running the standalone Files test application.
/// With no path, the dev instance can serve the Files command bus as before.
#[cfg(target_os = "windows")]
pub fn run_dev(path: Option<&str>) {
    win::run_with_initial(path);
}

/// Runs the component until shutdown.
pub fn run() {
    #[cfg(target_os = "windows")]
    win::run_with_initial(None);
}
