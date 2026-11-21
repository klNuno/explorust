//! USN_RECORD_V2 parsing: the records `FSCTL_ENUM_USN_DATA` and `FSCTL_READ_USN_JOURNAL` return
//! after their leading 8-byte cursor.

pub const REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// The MFT record number (the low 48 bits of the file reference).
    pub rec: u64,
    /// The sequence number (the high 16 bits of the file reference), for opening by id.
    pub seq: u16,
    pub parent: u64,
    pub usn: i64,
    pub reason: u32,
    pub attrs: u32,
    pub name: String,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

const MASK48: u64 = (1 << 48) - 1;

/// Parses the records of `buf` (without the cursor); stops at the first malformed one and
/// skips records of another major version.
pub fn parse(buf: &[u8], mut f: impl FnMut(Record)) {
    let mut o = 0usize;
    while o + 60 <= buf.len() {
        let Some(len) = u32_at(buf, o).map(|l| l as usize) else { break };
        if len < 60 || o + len > buf.len() {
            break;
        }
        let r = &buf[o..o + len];
        if u16_at(r, 4) == Some(2) {
            let (Some(frn), Some(parent), Some(usn), Some(reason), Some(attrs), Some(nl), Some(no)) =
                (u64_at(r, 8), u64_at(r, 16), u64_at(r, 24), u32_at(r, 40), u32_at(r, 52), u16_at(r, 56), u16_at(r, 58))
            else {
                break;
            };
            let (nl, no) = (nl as usize, no as usize);
            if no + nl <= len {
                let units: Vec<u16> = r[no..no + nl].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                f(Record { rec: frn & MASK48, seq: (frn >> 48) as u16, parent: parent & MASK48, usn: usn as i64, reason, attrs, name: String::from_utf16_lossy(&units) });
            }
        }
        o += len;
    }
}

/// Builds one V2 record, for tests and the bench.
pub fn encode(rec: u64, parent: u64, usn: i64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let len = (60 + units.len() * 2).div_ceil(8) * 8;
    let mut b = vec![0u8; len];
    b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    b[8..16].copy_from_slice(&(rec | (7 << 48)).to_le_bytes());
    b[16..24].copy_from_slice(&(parent | (3 << 48)).to_le_bytes());
    b[24..32].copy_from_slice(&usn.to_le_bytes());
    b[40..44].copy_from_slice(&reason.to_le_bytes());
    b[52..56].copy_from_slice(&attrs.to_le_bytes());
    b[56..58].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
    b[58..60].copy_from_slice(&60u16.to_le_bytes());
    for (i, u) in units.iter().enumerate() {
        b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut buf = encode(40, 5, 1234, REASON_FILE_CREATE, ATTR_DIRECTORY, "Projets");
        buf.extend(encode(41, 40, 1300, REASON_RENAME_NEW_NAME, 0x20, "notes été.txt"));
        let mut got = Vec::new();
        parse(&buf, |r| got.push(r));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], Record { rec: 40, seq: 7, parent: 5, usn: 1234, reason: REASON_FILE_CREATE, attrs: ATTR_DIRECTORY, name: "Projets".into() });
        assert_eq!(got[1].name, "notes été.txt");
        assert_eq!(got[1].parent, 40);
    }
}
