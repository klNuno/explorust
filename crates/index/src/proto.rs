//! The pipe's wire format, little endian. A request: max hits (u16), query length (u16), the query
//! in UTF-8. A reply: hit count (u32), the service's time in microseconds (u32), names indexed
//! (u32), then per hit: flags (u8, bit 0 folder, bit 1 noisy), rank (u8), path length (u16), path.
//! The high bit of max hits requests V2. A V2 reply sets the high bit of hit count and appends
//! last write time (u64, FILETIME ticks) to each hit. A V1 service clamps max hits to 200 and
//! answers in V1; a V1 client only ever requests and receives V1.

use std::io::{self, Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

const V2_REQUEST: u16 = 1 << 15;
const V2_REPLY: u32 = 1 << 31;
const MAX_HITS: u16 = 200;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub folder: bool,
    pub noisy: bool,
    /// 0 name prefix, 1 word prefix, 2 inside the name.
    pub rank: u8,
    /// Last write time in FILETIME ticks, 0 when unknown or received from a V1 service.
    pub modified: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub hits: Vec<Hit>,
    pub micros: u32,
    pub names: u32,
}

pub fn write_request(w: &mut impl Write, q: &str, max: u16, version: Version) -> io::Result<()> {
    let q = &q.as_bytes()[..q.len().min(1024)];
    let mut b = Vec::with_capacity(4 + q.len());
    let max = max.min(MAX_HITS) | if version == Version::V2 { V2_REQUEST } else { 0 };
    b.extend_from_slice(&max.to_le_bytes());
    b.extend_from_slice(&(q.len() as u16).to_le_bytes());
    b.extend_from_slice(q);
    w.write_all(&b)?;
    w.flush()
}

pub fn read_request(r: &mut impl Read) -> io::Result<(String, u16, Version)> {
    let mut h = [0u8; 4];
    r.read_exact(&mut h)?;
    let max = u16::from_le_bytes([h[0], h[1]]);
    let n = u16::from_le_bytes([h[2], h[3]]) as usize;
    if n > 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "query too long"));
    }
    let mut q = vec![0u8; n];
    r.read_exact(&mut q)?;
    let version = if max & V2_REQUEST != 0 { Version::V2 } else { Version::V1 };
    Ok((String::from_utf8_lossy(&q).into_owned(), (max & !V2_REQUEST).min(MAX_HITS), version))
}

pub fn write_reply(w: &mut impl Write, r: &Reply, version: Version) -> io::Result<()> {
    let mut b = Vec::with_capacity(12 + r.hits.len() * 64);
    let count = r.hits.len() as u32 | if version == Version::V2 { V2_REPLY } else { 0 };
    b.extend_from_slice(&count.to_le_bytes());
    b.extend_from_slice(&r.micros.to_le_bytes());
    b.extend_from_slice(&r.names.to_le_bytes());
    for h in &r.hits {
        let p = &h.path.as_bytes()[..h.path.len().min(u16::MAX as usize)];
        b.push(h.folder as u8 | (h.noisy as u8) << 1);
        b.push(h.rank);
        b.extend_from_slice(&(p.len() as u16).to_le_bytes());
        b.extend_from_slice(p);
        if version == Version::V2 {
            b.extend_from_slice(&h.modified.to_le_bytes());
        }
    }
    w.write_all(&b)?;
    w.flush()
}

pub fn read_reply(r: &mut impl Read) -> io::Result<Reply> {
    let mut h = [0u8; 12];
    r.read_exact(&mut h)?;
    let count = u32::from_le_bytes(h[0..4].try_into().unwrap_or_default());
    let n = (count & !V2_REPLY) as usize;
    if n > usize::from(MAX_HITS) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too many hits"));
    }
    let micros = u32::from_le_bytes(h[4..8].try_into().unwrap_or_default());
    let names = u32::from_le_bytes(h[8..12].try_into().unwrap_or_default());
    let mut hits = Vec::with_capacity(n);
    for _ in 0..n {
        let mut e = [0u8; 4];
        r.read_exact(&mut e)?;
        let len = u16::from_le_bytes([e[2], e[3]]) as usize;
        let mut p = vec![0u8; len];
        r.read_exact(&mut p)?;
        let mut modified = [0u8; 8];
        if count & V2_REPLY != 0 {
            r.read_exact(&mut modified)?;
        }
        hits.push(Hit { path: String::from_utf8_lossy(&p).into_owned(), folder: e[0] & 1 != 0, noisy: e[0] & 2 != 0, rank: e[1], modified: u64::from_le_bytes(modified) });
    }
    Ok(Reply { hits, micros, names })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut b = Vec::new();
        write_request(&mut b, "rapport é", 24, Version::V2).unwrap();
        assert_eq!(read_request(&mut b.as_slice()).unwrap(), ("rapport é".to_string(), 24, Version::V2));
        let r = Reply {
            hits: vec![
                Hit { path: r"C:\Users\a\Rapport.docx".into(), folder: false, noisy: false, rank: 0, modified: 133_800_000_000_000_000 },
                Hit { path: r"C:\Users\a\AppData".into(), folder: true, noisy: true, rank: 2, modified: 133_700_000_000_000_000 },
            ],
            micros: 812,
            names: 312_000,
        };
        let mut b = Vec::new();
        write_reply(&mut b, &r, Version::V2).unwrap();
        assert_eq!(read_reply(&mut b.as_slice()).unwrap(), r);
    }

    #[test]
    fn legacy_request_and_reply_layouts() {
        let one_hit = b"\x01\0\0\0\x02\0\0\0\x03\0\0\0\0\0\x01\0a";
        let (_, _, version) = read_request(&mut b"\x18\0\x01\0a".as_slice()).unwrap();
        let mut reply = read_reply(&mut one_hit.as_slice()).unwrap();
        assert_eq!(reply.hits[0].modified, 0);
        reply.hits[0].modified = 42;
        let mut wire = Vec::new();
        write_reply(&mut wire, &reply, version).unwrap();
        assert_eq!(wire, one_hit);
        wire.clear();
        write_request(&mut wire, "a", 24, Version::V2).unwrap();
        assert_eq!(u16::from_le_bytes([wire[0], wire[1]]).min(200), 200);
        assert_eq!(&wire[2..], b"\x01\0a");
    }
}
