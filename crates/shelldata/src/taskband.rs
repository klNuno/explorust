//! The taskbar pins as Windows stores them: `HKCU\...\Explorer\Taskband`, value `Favorites`.
//! Each entry is one byte (0), a little-endian u32 length, then an absolute ITEMIDLIST of that
//! length: AppsFolder items for packaged apps, `User Pinned\TaskBar` shortcuts for the others.

/// The PIDLs of the pins, in the bar's order. Entries that do not parse end the list.
pub fn pin_pidls(blob: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while o + 5 <= blob.len() {
        let n = u32::from_le_bytes([blob[o + 1], blob[o + 2], blob[o + 3], blob[o + 4]]) as usize;
        let Some(pidl) = blob.get(o + 5..o + 5 + n) else { break };
        if !valid_pidl(pidl) {
            break;
        }
        out.push(pidl.to_vec());
        o += 5 + n;
    }
    out
}

/// Whether `p` is a chain of SHITEMIDs ending with the 2-byte zero terminator.
pub(crate) fn valid_pidl(p: &[u8]) -> bool {
    let mut q = 0usize;
    while q + 2 <= p.len() {
        let cb = u16::from_le_bytes([p[q], p[q + 1]]) as usize;
        if cb == 0 {
            return q + 2 <= p.len() && q > 0;
        }
        if cb < 2 {
            return false;
        }
        q += cb;
    }
    false
}

/// File name of the shortcut represented by a filesystem pin.
pub fn shortcut_name(name: &str) -> Option<&str> {
    const EXT: &str = ".lnk";
    if name.get(name.len().checked_sub(EXT.len())?..)?.eq_ignore_ascii_case(EXT) {
        return Some(name);
    }
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".tmp")?;
    let (original, suffix) = stem.rsplit_once("~rf")?;
    (original.ends_with(EXT) && !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| &name[..original.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ids: &[&[u8]]) -> Vec<u8> {
        let mut pidl = Vec::new();
        for id in ids {
            pidl.extend_from_slice(&((id.len() + 2) as u16).to_le_bytes());
            pidl.extend_from_slice(id);
        }
        pidl.extend_from_slice(&[0, 0]);
        let mut e = vec![0u8];
        e.extend_from_slice(&(pidl.len() as u32).to_le_bytes());
        e.extend_from_slice(&pidl);
        e
    }

    #[test]
    fn reads_entries_in_order() {
        let mut blob = entry(&[b"root", b"app-one"]);
        blob.extend(entry(&[b"lnk"]));
        let p = pin_pidls(&blob);
        assert_eq!(p.len(), 2);
        assert_eq!(&p[0][2..6], b"root");
        assert_eq!(&p[1][2..5], b"lnk");
    }

    #[test]
    fn stops_at_garbage() {
        let mut blob = entry(&[b"one"]);
        blob.extend_from_slice(&[0, 9, 0, 0, 0, 1, 2]);
        assert_eq!(pin_pidls(&blob).len(), 1);
        assert!(pin_pidls(&[]).is_empty());
        assert!(pin_pidls(&[0, 2, 0, 0, 0, 0, 0]).is_empty());
    }

    #[test]
    fn safe_save_pin_points_to_the_original_shortcut() {
        assert_eq!(shortcut_name("Mullvad Browser.lnk~RF21949e.TMP"), Some("Mullvad Browser.lnk"));
        assert_eq!(shortcut_name("Accshift.LNK"), Some("Accshift.LNK"));
        assert_eq!(shortcut_name("Tombstones"), None);
        assert_eq!(shortcut_name("random.TMP"), None);
        assert_eq!(shortcut_name("brouillon.lnk~RFnotes.TMP"), None);
    }
}
