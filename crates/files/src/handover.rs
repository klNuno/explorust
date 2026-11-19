//! The path a second process (`explorust-open.exe`) hands to the resident File Explorer through
//! `WM_COPYDATA`: `dwData` is `MAGIC`, the payload the UTF-16 path with its terminating NUL.

/// `dwData` of the handover: "EXPF".
pub const MAGIC: usize = 0x4558_5046;

/// The UTF-16 payload for `path`, NUL-terminated.
pub fn encode(path: &str) -> Vec<u16> {
    path.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The path in a payload of `bytes`, `None` when it is not UTF-16 or empty.
pub fn decode(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let s = String::from_utf16(&units[..end]).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(u: &[u16]) -> Vec<u8> {
        u.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn round_trips_a_path() {
        let p = "C:\\Users\\agent\\Téléchargements";
        assert_eq!(decode(&bytes(&encode(p))).as_deref(), Some(p));
    }

    #[test]
    fn rejects_empty_and_odd() {
        assert_eq!(decode(&bytes(&encode(""))), None);
        assert_eq!(decode(&[0x41]), None);
        assert_eq!(decode(&bytes(&encode("  "))), None);
    }

    #[test]
    fn accepts_a_payload_without_nul() {
        assert_eq!(decode(&bytes(&[0x43, 0x3a])).as_deref(), Some("C:"));
    }
}
