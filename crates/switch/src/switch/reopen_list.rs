use std::collections::HashSet;
use std::io;

#[derive(Debug, PartialEq, Eq)]
pub struct Folder {
    pub hwnd: isize,
    pub owner: u32,
    pub path: String,
}

/// ShellWindows can enumerate several tabs for one HWND. Keep each window's active folder,
/// including two different windows that happen to be on the same folder.
pub fn folders(entries: impl IntoIterator<Item = Folder>) -> Vec<Folder> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter(|folder| {
            !folder.path.is_empty() && !folder.path.contains('\0') && seen.insert(folder.hwnd)
        })
        .collect()
}

pub fn same_windows(saved: &[Folder], observed: impl Iterator<Item = (isize, u32)>) -> bool {
    let saved_handles: HashSet<_> = saved
        .iter()
        .map(|folder| (folder.hwnd, folder.owner))
        .collect();
    saved_handles == observed.collect()
}

pub fn handoff_paths(entries: &[Folder], earlier_receipts: &HashSet<(isize, u32)>) -> Vec<String> {
    entries
        .iter()
        .filter(|folder| !earlier_receipts.contains(&(folder.hwnd, folder.owner)))
        .map(|folder| folder.path.clone())
        .collect()
}

/// Keep the window identity alongside its parsing name for recovery in either process.
pub fn encode(entries: &[Folder]) -> String {
    use std::fmt::Write;
    let mut snapshot = String::new();
    for Folder { hwnd, owner, path } in entries {
        let _ = write!(snapshot, "{hwnd}\0{owner}\0{path}\0");
    }
    snapshot
}

pub fn decode(snapshot: &str) -> io::Result<Vec<Folder>> {
    let mut fields = snapshot.split_terminator('\0');
    let mut entries = Vec::new();
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Explorer folder snapshot",
        )
    };
    while let Some(hwnd) = fields.next() {
        let hwnd = hwnd.parse().map_err(|_| invalid())?;
        let owner = fields
            .next()
            .and_then(|pid| pid.parse().ok())
            .ok_or_else(invalid)?;
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or_else(invalid)?;
        entries.push(Folder {
            hwnd,
            owner,
            path: path.to_owned(),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(hwnd: isize, owner: u32, path: &str) -> Folder {
        Folder {
            hwnd,
            owner,
            path: path.to_owned(),
        }
    }

    #[test]
    fn recapture_when_a_folder_window_opens_or_closes() {
        let saved = [folder(1, 1, "a"), folder(2, 2, "b")];
        assert!(same_windows(&saved, [(2, 2), (1, 1)].into_iter()));
        assert!(!same_windows(&saved, [(1, 1), (2, 2), (3, 3)].into_iter()));
        assert!(!same_windows(&saved, [(1, 1)].into_iter()));
        assert!(!same_windows(&saved, [(1, 1), (2, 3)].into_iter()));
    }

    #[test]
    fn keeps_one_valid_parsing_name_per_window() {
        let local = r"C:\Windows\Web";
        let virtual_folder = "::{namespace}\\Échantillon\nsecond line";
        let entries = [
            folder(1, 1, ""),
            folder(1, 1, local),
            folder(1, 1, "duplicate"),
            folder(2, 1, local),
            folder(3, 2, virtual_folder),
            folder(4, 2, "bad\0path"),
        ];
        assert_eq!(
            folders(entries),
            [
                folder(1, 1, local),
                folder(2, 1, local),
                folder(3, 2, virtual_folder)
            ]
        );
    }

    #[test]
    fn recovery_snapshot_keeps_window_ids_and_parsing_names() {
        let entries = [
            folder(17, 21, r"C:\Windows\Web"),
            folder(23, 19, "::{namespace}\\É\nWeb"),
        ];
        assert_eq!(decode(&encode(&entries)).unwrap(), entries);
        assert!(decode("17\0").is_err());
        assert!(decode("").unwrap().is_empty());
    }

    #[test]
    fn skips_only_the_window_already_owned_by_an_earlier_receipt() {
        let entries = [folder(1, 10, "shared"), folder(2, 10, "shared"), folder(3, 20, "other")];
        let earlier = HashSet::from([(1, 10), (3, 99)]);
        assert_eq!(handoff_paths(&entries, &earlier), ["shared", "other"]);
    }
}
