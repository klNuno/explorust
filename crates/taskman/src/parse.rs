//! Parsing of NtQuerySystemInformation(SystemProcessInformation) buffers (x64 layout of
//! SYSTEM_PROCESS_INFORMATION), pure so it is tested off Windows.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawProc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub threads: u32,
    pub create: i64,
    /// User plus kernel time, 100 ns units.
    pub cpu: i64,
    pub private_ws: u64,
    pub io: u64,
    pub session: u32,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Walks the entries of `buf`; `base` is the address the buffer had when the kernel filled it,
/// so the image name pointers resolve to offsets inside it.
pub fn processes(buf: &[u8], base: usize) -> Vec<RawProc> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 256 <= buf.len() {
        let e = &buf[at..];
        let next = u32_at(e, 0) as usize;
        let len = u16::from_le_bytes([e[56], e[57]]) as usize;
        let ptr = u64_at(e, 64) as usize;
        let name = if ptr >= base && ptr - base + len <= buf.len() && len > 0 {
            let s = &buf[ptr - base..ptr - base + len];
            String::from_utf16_lossy(&s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
        } else if u64_at(e, 80) == 0 {
            "System Idle Process".into()
        } else {
            String::new()
        };
        out.push(RawProc {
            pid: u64_at(e, 80) as u32,
            ppid: u64_at(e, 88) as u32,
            name,
            threads: u32_at(e, 4),
            create: u64_at(e, 32) as i64,
            cpu: u64_at(e, 40) as i64 + u64_at(e, 48) as i64,
            private_ws: u64_at(e, 8),
            io: u64_at(e, 232) + u64_at(e, 240),
            session: u32_at(e, 100),
        });
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

/// The pid and engine type of a "\GPU Engine(*)" instance, as
/// `pid_1234_luid_0x0_0x0000C5E4_phys_0_eng_0_engtype_3D`.
pub fn gpu_instance(name: &str) -> Option<(u32, &str)> {
    let rest = name.strip_prefix("pid_")?;
    let pid = rest.split('_').next()?.parse().ok()?;
    let eng = name.rsplit_once("engtype_").map(|(_, e)| e).unwrap_or("");
    Some((pid, eng))
}

/// Startup entry state from its StartupApproved value: the first byte is even when enabled
/// (2 or 6), odd when disabled (3, 7); no value means enabled.
pub fn approved(value: Option<&[u8]>) -> bool {
    value.and_then(|v| v.first()).is_none_or(|b| b & 1 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(next: u32, pid: u64, ppid: u64, name_ptr: u64, name_len: u16) -> Vec<u8> {
        let mut e = vec![0u8; 256];
        e[0..4].copy_from_slice(&next.to_le_bytes());
        e[4..8].copy_from_slice(&7u32.to_le_bytes());
        e[8..16].copy_from_slice(&4096u64.to_le_bytes());
        e[40..48].copy_from_slice(&100u64.to_le_bytes());
        e[48..56].copy_from_slice(&50u64.to_le_bytes());
        e[56..58].copy_from_slice(&name_len.to_le_bytes());
        e[64..72].copy_from_slice(&name_ptr.to_le_bytes());
        e[80..88].copy_from_slice(&pid.to_le_bytes());
        e[88..96].copy_from_slice(&ppid.to_le_bytes());
        e[232..240].copy_from_slice(&10u64.to_le_bytes());
        e[240..248].copy_from_slice(&5u64.to_le_bytes());
        e
    }

    #[test]
    fn walks_entries_and_resolves_names() {
        let base = 0x1000usize;
        let mut buf = entry(256, 0, 0, 0, 0);
        let name: Vec<u8> = "a.exe".encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        buf.extend(entry(0, 42, 4, (base + 512) as u64, name.len() as u16));
        buf.extend(&name);
        let p = processes(&buf, base);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "System Idle Process");
        assert_eq!((p[1].pid, p[1].ppid, p[1].name.as_str()), (42, 4, "a.exe"));
        assert_eq!((p[1].cpu, p[1].private_ws, p[1].io, p[1].threads), (150, 4096, 15, 7));
    }

    #[test]
    fn gpu_names_and_startup_bytes() {
        assert_eq!(gpu_instance("pid_1234_luid_0x00000000_0x0000C5E4_phys_0_eng_0_engtype_3D"), Some((1234, "3D")));
        assert_eq!(gpu_instance("_Total"), None);
        assert!(approved(None) && approved(Some(&[2, 0])) && approved(Some(&[6])));
        assert!(!approved(Some(&[3, 0])) && !approved(Some(&[7])));
    }
}
