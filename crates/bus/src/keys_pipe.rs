//! Wire format shared by the shell and the keys host.

use crate::{Command, Owner};

#[cfg(target_os = "windows")]
mod client;
#[cfg(target_os = "windows")]
pub(crate) use client::post;
#[cfg(target_os = "windows")]
pub use client::{name, restore_batch, sibling_image};

pub const RECORD: usize = 16;
pub const BUFFER: usize = 64 * 1024;

pub(super) fn encode(cmd: Command, arg: isize) -> Option<[u8; RECORD]> {
    if cmd.owner() != Owner::Keys {
        return None;
    }
    let mut record = [0; RECORD];
    record[..4].copy_from_slice(&(cmd as u32).to_le_bytes());
    record[8..].copy_from_slice(&(arg as i64).to_le_bytes());
    Some(record)
}

pub fn decode(record: &[u8; RECORD]) -> Option<(Command, isize)> {
    let cmd = Command::from_u32(u32::from_le_bytes(record[..4].try_into().ok()?))?;
    if cmd.owner() != Owner::Keys || record[4..8] != [0; 4] {
        return None;
    }
    let arg = i64::from_le_bytes(record[8..].try_into().ok()?);
    Some((cmd, isize::try_from(arg).ok()?))
}

fn restore_records(windows: &[isize]) -> Option<Vec<u8>> {
    let &top = windows.last()?;
    if windows.len() >= BUFFER / RECORD {
        return None;
    }
    let mut records = Vec::with_capacity((windows.len() + 1) * RECORD);
    for &window in windows {
        records.extend_from_slice(&encode(Command::Restore, window)?);
    }
    records.extend_from_slice(&encode(Command::Activate, top)?);
    Some(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_fixed_width_and_only_accept_keys_commands() {
        let mut record = encode(Command::Minimize, -7).unwrap();
        assert_eq!(&record[..8], &[26, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode(&record), Some((Command::Minimize, -7)));
        record[4] = 1;
        assert_eq!(decode(&record), None);
        record[4] = 0;
        record[0] = Command::ShowDesktop as u8;
        assert_eq!(decode(&record), None);
        assert!(encode(Command::ShowDesktop, 1).is_none());
    }

    #[test]
    fn restore_message_ends_by_activating_the_last_window() {
        let records = restore_records(&[17, 23, 42]).unwrap();
        let actions: Vec<_> = records
            .chunks_exact(RECORD)
            .map(|b| decode(b.try_into().unwrap()))
            .collect();
        assert_eq!(
            actions,
            [
                Some((Command::Restore, 17)),
                Some((Command::Restore, 23)),
                Some((Command::Restore, 42)),
                Some((Command::Activate, 42)),
            ]
        );
        assert!(restore_records(&[]).is_none());
        assert!(restore_records(&[0; BUFFER / RECORD]).is_none());
    }
}
