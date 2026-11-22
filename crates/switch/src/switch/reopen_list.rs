use std::collections::HashSet;
use std::io;

/// ShellWindows can enumerate several tabs for one HWND. Keep each window's active folder,
/// including two different windows that happen to be on the same folder.
pub fn folders(entries: impl IntoIterator<Item = (isize, String)>) -> Vec<(isize, String)> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter_map(|(hwnd, folder)| {
            (!folder.is_empty() && !folder.contains('\0') && seen.insert(hwnd))
                .then_some((hwnd, folder))
        })
        .collect()
}

/// Keep the window identity alongside its parsing name for recovery in either process.
pub fn encode(entries: &[(isize, String)]) -> String {
    use std::fmt::Write;
    let mut snapshot = String::new();
    for (hwnd, path) in entries {
        let _ = write!(snapshot, "{hwnd}\0{path}\0");
    }
    snapshot
}

pub fn decode(snapshot: &str) -> io::Result<Vec<(isize, String)>> {
    let mut fields = snapshot.split_terminator('\0');
    let mut entries = Vec::new();
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid Explorer folder snapshot");
    while let Some(hwnd) = fields.next() {
        let hwnd = hwnd.parse().map_err(|_| invalid())?;
        let path = fields.next().filter(|path| !path.is_empty()).ok_or_else(invalid)?;
        entries.push((hwnd, path.to_owned()));
    }
    Ok(entries)
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
            [(1, local.to_owned()), (2, local.to_owned()), (3, virtual_folder.to_owned())]
        );
    }

    #[test]
    fn recovery_snapshot_keeps_window_ids_and_parsing_names() {
        let entries = [(17, r"C:\Windows\Web".to_owned()), (23, "::{namespace}\\É\nWeb".to_owned())];
        assert_eq!(decode(&encode(&entries)).unwrap(), entries);
        assert!(decode("17\0").is_err());
        assert!(decode("").unwrap().is_empty());
    }
}
