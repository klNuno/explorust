//! `TranscodedImageCache` (HKCU\Control Panel\Desktop, REG_BINARY): the header Windows writes with
//! its transcoded copy of the wallpaper (`Themes\TranscodedWallpaper`). The source path is stored
//! as NUL-terminated UTF-16 from byte 24. The copy is only used when it was made from the current
//! wallpaper: without explorer nothing refreshes it when the wallpaper changes.

const PATH_AT: usize = 24;

/// The source path recorded in the cache blob.
pub fn source(blob: &[u8]) -> Option<String> {
    let units: Vec<u16> = blob.get(PATH_AT..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let n = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    (n > 0).then(|| String::from_utf16_lossy(&units[..n]))
}

/// Whether the transcoded copy was made from `wallpaper` (paths compared case-insensitively).
pub fn matches(blob: &[u8], wallpaper: &str) -> bool {
    source(blob).is_some_and(|s| !wallpaper.is_empty() && s.to_lowercase() == wallpaper.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(path: &str) -> Vec<u8> {
        let mut b = vec![0u8; PATH_AT];
        b[0] = 0x7a;
        for u in path.encode_utf16().chain([0]) {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b.extend_from_slice(&[0u8; 16]);
        b
    }

    #[test]
    fn reads_the_source_path() {
        assert_eq!(source(&blob(r"C:\Pics\a.jpg")).as_deref(), Some(r"C:\Pics\a.jpg"));
        assert_eq!(source(&[0u8; 10]), None);
        assert_eq!(source(&blob("")), None);
    }

    #[test]
    fn matches_only_the_current_wallpaper() {
        let b = blob(r"C:\Pics\A.jpg");
        assert!(matches(&b, r"c:\pics\a.jpg"));
        assert!(!matches(&b, r"C:\Windows\Web\Wallpaper\Windows\img0.jpg"));
        assert!(!matches(&b, ""));
    }
}
