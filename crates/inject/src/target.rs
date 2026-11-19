//! Which windows the injector styles, and the kill switch: plain logic, tested on any OS.

/// What a window is to the injector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Kind {
    /// A common item dialog (IFileOpenDialog / IFileSaveDialog) of another app: dark theme on its
    /// controls and Mica on its frame.
    FileDialog = 1,
    /// The frame that hosts Settings (SystemSettings.exe): acrylic backdrop and dark caption.
    Settings = 2,
}

impl Kind {
    pub fn from_usize(v: usize) -> Option<Kind> {
        match v {
            1 => Some(Kind::FileDialog),
            2 => Some(Kind::Settings),
            _ => None,
        }
    }
}

/// The dialog class of the common item dialog's top-level window.
pub const DIALOG_CLASS: &str = "#32770";
/// A descendant only the common item dialog (Vista and later) has: its DirectUI view host.
pub const DIALOG_MARK: &str = "DUIViewWndClassName";
pub const CORE_WINDOW: &str = "Windows.UI.Core.CoreWindow";
pub const FRAME_WINDOW: &str = "ApplicationFrameWindow";
pub const SETTINGS_EXE: &str = "systemsettings.exe";

/// The file name of an image path, lower case: `C:\Windows\ImmersiveControlPanel\SystemSettings.exe`
/// gives `systemsettings.exe`.
pub fn exe_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase()
}

/// A shown top-level window of another process: is it a target? `has_mark` says whether it has a
/// `DUIViewWndClassName` descendant; `settings_child` whether a CoreWindow of SystemSettings.exe sits
/// in it (a frame) or it is that CoreWindow itself.
pub fn classify(class: &str, exe: &str, has_mark: bool, settings_child: bool) -> Option<Kind> {
    if class == DIALOG_CLASS && has_mark {
        return Some(Kind::FileDialog);
    }
    if class == FRAME_WINDOW && settings_child {
        return Some(Kind::Settings);
    }
    if class == CORE_WINDOW && exe_name(exe) == SETTINGS_EXE {
        return Some(Kind::Settings);
    }
    None
}

/// The kill switch: a line `inject` in `%LOCALAPPDATA%\explorust\parts-off.txt`.
pub fn switched_off(parts_off: &str) -> bool {
    parts_off.lines().any(|l| l.trim().eq_ignore_ascii_case("inject"))
}

/// The theme a child control of the dialog takes in dark mode, by window class (the names are
/// uxtheme's dark class lists that Windows' own dialogs use).
pub fn dark_theme_of(class: &str) -> Option<&'static str> {
    match class {
        "SysTreeView32" | "SysListView32" | "DirectUIHWND" | "ScrollBar" | "Button" | "SysHeader32"
        | "ToolbarWindow32" | "ReBarWindow32" | "Breadcrumb Parent" | "NamespaceTreeControl" => {
            Some("DarkMode_Explorer")
        }
        "Edit" | "ComboBox" | "ComboBoxEx32" => Some("DarkMode_CFD"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_dialog_needs_its_mark() {
        assert_eq!(classify("#32770", "C:\\x\\notepad.exe", true, false), Some(Kind::FileDialog));
        assert_eq!(classify("#32770", "C:\\x\\notepad.exe", false, false), None);
    }

    #[test]
    fn settings_by_core_window_or_frame() {
        let p = "C:\\Windows\\ImmersiveControlPanel\\SystemSettings.exe";
        assert_eq!(classify(CORE_WINDOW, p, false, false), Some(Kind::Settings));
        assert_eq!(classify(CORE_WINDOW, "C:\\x\\CalculatorApp.exe", false, false), None);
        assert_eq!(classify(FRAME_WINDOW, "C:\\x\\ApplicationFrameHost.exe", false, true), Some(Kind::Settings));
        assert_eq!(classify(FRAME_WINDOW, "C:\\x\\ApplicationFrameHost.exe", false, false), None);
    }

    #[test]
    fn exe_names() {
        assert_eq!(exe_name("C:\\Windows\\ImmersiveControlPanel\\SystemSettings.exe"), "systemsettings.exe");
        assert_eq!(exe_name("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn kill_switch() {
        assert!(switched_off("files\r\n inject \r\n"));
        assert!(!switched_off("files\ninjector\n"));
        assert!(!switched_off(""));
    }

    #[test]
    fn kinds_round_trip() {
        for k in [Kind::FileDialog, Kind::Settings] {
            assert_eq!(Kind::from_usize(k as usize), Some(k));
        }
        assert_eq!(Kind::from_usize(0), None);
    }

    #[test]
    fn dark_classes() {
        assert_eq!(dark_theme_of("Edit"), Some("DarkMode_CFD"));
        assert_eq!(dark_theme_of("SysTreeView32"), Some("DarkMode_Explorer"));
        assert_eq!(dark_theme_of("Static"), None);
    }
}
