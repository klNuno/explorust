use std::collections::HashSet;

/// ShellWindows can enumerate several tabs for one HWND. Keep each window's active folder,
/// including two different windows that happen to be on the same folder.
pub fn folders(entries: impl IntoIterator<Item = (isize, String)>) -> Vec<String> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|(hwnd, folder)| {
            (!folder.is_empty() && !folder.contains('\0') && seen.insert(hwnd)).then_some(folder)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_one_valid_parsing_name_per_window() {
        let local = r"C:\Windows\Web";
        let virtual_folder = "::{namespace}\\Échantillon\nsecond line";
        let entries = [
            (1, ""),
            (1, local),
            (1, "duplicate"),
            (2, local),
            (3, virtual_folder),
            (4, "bad\0path"),
        ];
        assert_eq!(
            folders(entries.map(|(h, folder)| (h, folder.to_owned()))),
            [local, local, virtual_folder]
        );
    }
}
