//! StartupApproved states and the documented RunOnce name prefixes.

pub fn disabled(record: &[u8]) -> bool {
    matches!(
        record
            .get(..4)
            .and_then(|p| p.try_into().ok())
            .map(u32::from_le_bytes),
        Some(3 | 7)
    )
}

pub fn run_once_flags(name: &[u16]) -> (bool, bool) {
    let mut deferred = false;
    let mut safe_mode = false;
    for &unit in name {
        match unit {
            33 => deferred = true,  // !
            42 => safe_mode = true, // *
            _ => break,
        }
    }
    (deferred, safe_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_records_distinguish_both_disabled_states_from_enabled() {
        assert!(!disabled(&2u32.to_le_bytes()));
        assert!(!disabled(&6u32.to_le_bytes()));
        assert!(disabled(&3u32.to_le_bytes()));
        assert!(disabled(&7u32.to_le_bytes()));
        assert!(!disabled(&[3]));
    }

    #[test]
    fn run_once_prefixes_can_appear_together_in_either_order() {
        for name in ["!*example", "*!example"] {
            assert_eq!(
                run_once_flags(&name.encode_utf16().collect::<Vec<_>>()),
                (true, true)
            );
        }
        assert_eq!(run_once_flags(&[33]), (true, false));
        assert_eq!(run_once_flags(&[42]), (false, true));
        assert_eq!(run_once_flags(&[65, 33, 42]), (false, false));
    }
}
