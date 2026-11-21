//! One NTFS volume: its handle, the USN journal, the MFT enumeration and the journal follower.

use crate::log;
use fsindex::table::{RecState, Table};
use fsindex::usn::{self, Record};
use std::sync::RwLock;
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows_core::HSTRING;

const DRIVE_FIXED: u32 = 3;
/// Metadata files ($MFT, $LogFile, the root itself) sit below record 16.
const FIRST_USER_RECORD: u64 = 16;

/// The letters of the fixed NTFS volumes.
pub fn fixed_ntfs() -> Vec<char> {
    // SAFETY: plain queries with local buffers.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| (b'A' + i) as char)
        .filter(|&c| {
            let root = HSTRING::from(format!("{c}:\\"));
            let mut fs = [0u16; 16];
            // SAFETY: as above.
            unsafe {
                GetDriveTypeW(&root) == DRIVE_FIXED
                    && GetVolumeInformationW(&root, None, None, None, None, Some(&mut fs)).is_ok()
                    && String::from_utf16_lossy(&fs).trim_end_matches('\0') == "NTFS"
            }
        })
        .collect()
}

pub struct Volume {
    pub letter: char,
    h: HANDLE,
}

// SAFETY: the volume handle is used from the follower thread only after the load.
unsafe impl Send for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: closes the handle opened in `open`.
        let _ = unsafe { CloseHandle(self.h) };
    }
}

fn ioctl<I>(h: HANDLE, code: u32, input: &I, out: &mut [u8]) -> windows_core::Result<usize> {
    let mut got = 0u32;
    // SAFETY: the input struct and the output buffer outlive the synchronous call.
    unsafe {
        DeviceIoControl(
            h,
            code,
            Some(input as *const I as *const _),
            size_of::<I>() as u32,
            Some(out.as_mut_ptr().cast()),
            out.len() as u32,
            Some(&mut got),
            None,
        )?;
    }
    Ok(got as usize)
}

/// FileHardLinkInformation (FILE_INFORMATION_CLASS 46): every name of a file with its folder.
const FILE_HARD_LINK_INFORMATION: u32 = 46;

windows_core::link!("ntdll.dll" "system" fn NtQueryInformationFile(file: HANDLE, iosb: *mut [usize; 2], info: *mut core::ffi::c_void, len: u32, class: u32) -> i32);

/// The names of file `frn` (full reference, sequence included) as (parent record, name).
fn hard_links(volume: HANDLE, frn: u64, buf: &mut Vec<u8>) -> Option<Vec<(u32, String)>> {
    let id = FILE_ID_DESCRIPTOR { dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32, Type: FileIdType, Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 } };
    // SAFETY: opens by id for attributes only; the handle is closed below.
    let h = unsafe {
        OpenFileById(volume, &id, FILE_READ_ATTRIBUTES.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, FILE_FLAG_BACKUP_SEMANTICS).ok()?
    };
    let mut iosb = [0usize; 2];
    let mut st = 0;
    for _ in 0..2 {
        // SAFETY: the buffer outlives the call; the class fills FILE_LINKS_INFORMATION.
        st = unsafe { NtQueryInformationFile(h, &mut iosb, buf.as_mut_ptr().cast(), buf.len() as u32, FILE_HARD_LINK_INFORMATION) };
        // Too many links for the buffer: NTFS reports the size it needs in BytesNeeded.
        let needed = buf.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        if st >= 0 || needed <= buf.len() || needed > 1 << 20 {
            break;
        }
        buf.resize(needed, 0);
    }
    // SAFETY: the handle opened above.
    let _ = unsafe { CloseHandle(h) };
    if st < 0 {
        return None;
    }
    // FILE_LINKS_INFORMATION: BytesNeeded u32, EntriesReturned u32, then entries of
    // NextEntryOffset u32, (pad) ParentFileId i64 at 8, FileNameLength u32 at 16, FileName at 20.
    let u32_at = |o: usize| buf.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let n = u32_at(4)? as usize;
    let (mut o, mut out) = (8usize, Vec::with_capacity(n));
    for _ in 0..n {
        let next = u32_at(o)? as usize;
        let parent = u64::from_le_bytes(buf.get(o + 8..o + 16)?.try_into().ok()?) & ((1 << 48) - 1);
        let len = u32_at(o + 16)? as usize;
        let units: Vec<u16> = buf.get(o + 20..o + 20 + len * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        out.push((parent.min(u32::MAX as u64) as u32, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        o += next;
    }
    Some(out)
}

/// Gives each file in `frns` (whose name is inside a pruned tree, or whose kept name may be gone)
/// its first name outside the pruned trees, if it has one: System32's binaries are hard links whose
/// first name is in WinSxS; a kept name with no link left outside is dropped. The links are read
/// without the lock, so queries and the journal go on meanwhile; a result is written only into the
/// same table and only when its record still holds what it held before the read (the journal wins
/// any change made in between, `Table::touched`). `id` is the table the files were taken from.
/// Returns (files looked at, names added, the files whose lookup failed).
pub fn add_links(volume: HANDLE, frns: &[u64], table: &RwLock<Table>, id: u64) -> (usize, usize, Vec<u64>) {
    let mut buf = vec![0u8; 8 << 10];
    let (mut added, mut failed) = (0, Vec::new());
    {
        let mut t = table.write().unwrap_or_else(|p| p.into_inner());
        if t.id() != id {
            return (0, 0, failed);
        }
        t.watch_touches(true);
    }
    for chunk in frns.chunks(256) {
        let before: Vec<RecState> = {
            let t = table.read().unwrap_or_else(|p| p.into_inner());
            chunk.iter().map(|&f| t.state((f & ((1 << 48) - 1)) as u32)).collect()
        };
        let mut found = Vec::new();
        for (&frn, was) in chunk.iter().zip(before) {
            match hard_links(volume, frn, &mut buf) {
                Some(links) => found.push(((frn & ((1 << 48) - 1)) as u32, was, links)),
                None => failed.push(frn),
            }
        }
        if found.is_empty() {
            continue;
        }
        let mut t = table.write().unwrap_or_else(|p| p.into_inner());
        if t.id() != id {
            break;
        }
        for (rec, was, links) in found {
            if t.state(rec) != was || t.touched(rec) {
                continue;
            }
            match links.into_iter().find(|(p, _)| !t.pruned_parent(*p)) {
                Some((parent, name)) => {
                    if !matches!(&was, RecState::Named(p, n) if *p == parent && *n == name) {
                        t.upsert(rec, parent, &name, false, false, 0);
                        added += 1;
                    }
                }
                None => t.prune(rec),
            }
        }
    }
    let mut t = table.write().unwrap_or_else(|p| p.into_inner());
    if t.id() == id {
        t.watch_touches(false);
    }
    (frns.len(), added, failed)
}

fn apply(t: &mut Table, r: &Record) {
    if r.rec < FIRST_USER_RECORD || r.rec > u32::MAX as u64 - 1 {
        return;
    }
    t.touch(r.rec as u32);
    if r.reason & usn::REASON_FILE_DELETE != 0 {
        t.remove(r.rec as u32);
    } else if r.reason & usn::REASON_RENAME_OLD_NAME == 0 {
        let dir = r.attrs & usn::ATTR_DIRECTORY != 0;
        let noisy = r.attrs & (usn::ATTR_HIDDEN | usn::ATTR_SYSTEM) != 0;
        let parent = r.parent.min(u32::MAX as u64) as u32;
        t.upsert(r.rec as u32, parent, &r.name, dir, noisy, r.usn);
        // A kept name may be gone: renamed into a pruned tree, or one of its hard links removed.
        if !dir && (r.reason & usn::REASON_HARD_LINK_CHANGE != 0 || (r.reason & usn::REASON_RENAME_NEW_NAME != 0 && t.pruned_parent(parent))) {
            t.recheck_links(r.rec as u32);
        }
    }
}

impl Volume {
    pub fn open(letter: char) -> windows_core::Result<Volume> {
        let path = HSTRING::from(format!(r"\\.\{letter}:"));
        // SAFETY: opens the volume for reads; the handle is closed on drop.
        let h = unsafe {
            CreateFileW(&path, GENERIC_READ.0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)?
        };
        Ok(Volume { letter, h })
    }

    pub fn journal(&self) -> windows_core::Result<USN_JOURNAL_DATA_V0> {
        let mut out = [0u8; size_of::<USN_JOURNAL_DATA_V0>() + 64];
        let q = match ioctl(self.h, FSCTL_QUERY_USN_JOURNAL, &(), &mut out) {
            Err(e) if e.code() == ERROR_JOURNAL_NOT_ACTIVE.to_hresult() => {
                let c = CREATE_USN_JOURNAL_DATA { MaximumSize: 32 << 20, AllocationDelta: 8 << 20 };
                ioctl(self.h, FSCTL_CREATE_USN_JOURNAL, &c, &mut [])?;
                log::line(&format!("{}: USN journal created", self.letter));
                ioctl(self.h, FSCTL_QUERY_USN_JOURNAL, &(), &mut out)
            }
            r => r,
        };
        q?;
        // SAFETY: the driver filled a USN_JOURNAL_DATA (V0 is its prefix).
        Ok(unsafe { std::ptr::read_unaligned(out.as_ptr() as *const USN_JOURNAL_DATA_V0) })
    }

    pub fn handle(&self) -> HANDLE {
        self.h
    }

    /// Every name in the MFT up to `high` into a new table, and the full references of the files
    /// left out with the pruned trees (for `add_links`).
    pub fn load(&self, high: i64) -> windows_core::Result<(Table, Vec<u64>)> {
        let mut t = Table::new();
        let mut seqs: Vec<u16> = Vec::new();
        let mut med = MFT_ENUM_DATA_V0 { StartFileReferenceNumber: 0, LowUsn: 0, HighUsn: high };
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let got = match ioctl(self.h, FSCTL_ENUM_USN_DATA, &med, &mut buf) {
                Ok(n) => n,
                Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => break,
                Err(e) => return Err(e),
            };
            if got <= 8 {
                break;
            }
            med.StartFileReferenceNumber = u64::from_le_bytes(buf[..8].try_into().unwrap_or_default());
            usn::parse(&buf[8..got], |r| {
                if let Some(i) = usize::try_from(r.rec).ok().filter(|&i| i < u32::MAX as usize) {
                    if seqs.len() <= i {
                        seqs.resize(i + 1, 0);
                    }
                    seqs[i] = r.seq;
                }
                apply(&mut t, &r)
            });
        }
        let pruned = t.settle();
        t.shrink();
        let files: Vec<u64> = t.take_pruned_files().into_iter().map(|r| r as u64 | (seqs.get(r as usize).copied().unwrap_or(0) as u64) << 48).collect();
        log::line(&format!("{}: {pruned} names inside WinSxS left out ({} files)", self.letter, files.len()));
        Ok((t, files))
    }

    /// Applies the journal from `next` on, blocking until changes come; returns on an error
    /// (journal deleted or wrapped past `next`), after which the caller reloads.
    pub fn follow(&self, journal_id: u64, mut next: i64, table: &RwLock<Table>) -> windows_core::Error {
        let mut buf = vec![0u8; 64 << 10];
        let mut changes = 0u64;
        let mut since = Instant::now();
        let mut seq_of: std::collections::HashMap<u32, u16> = std::collections::HashMap::new();
        let (mut retry, mut tries) = (Vec::new(), std::collections::HashMap::<u64, u8>::new());
        loop {
            let rd = READ_USN_JOURNAL_DATA_V0 {
                StartUsn: next,
                ReasonMask: u32::MAX,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 1,
                UsnJournalID: journal_id,
            };
            let got = match ioctl(self.h, FSCTL_READ_USN_JOURNAL, &rd, &mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            if got < 8 {
                continue;
            }
            next = i64::from_le_bytes(buf[..8].try_into().unwrap_or_default());
            if got > 8 {
                let mut pruned = std::mem::take(&mut retry);
                let id;
                {
                    let mut t = table.write().unwrap_or_else(|p| p.into_inner());
                    id = t.id();
                    usn::parse(&buf[8..got], |r| {
                        changes += 1;
                        apply(&mut t, &r);
                        if r.rec <= u32::MAX as u64 {
                            seq_of.insert(r.rec as u32, r.seq);
                        }
                    });
                    for rec in t.take_pruned_files() {
                        pruned.push(rec as u64 | (seq_of.get(&rec).copied().unwrap_or(0) as u64) << 48);
                    }
                    seq_of.clear();
                }
                // A file added inside WinSxS by an update: its System32 link, if any.
                // A lookup that failed (file busy, buffer) is tried again with the next batch, up
                // to 3 times.
                if !pruned.is_empty() {
                    let failed = add_links(self.h, &pruned, table, id).2;
                    for frn in pruned.iter().filter(|f| !failed.contains(f)) {
                        tries.remove(frn);
                    }
                    for frn in failed {
                        let n = tries.entry(frn).or_insert(0);
                        *n += 1;
                        if *n < 3 {
                            retry.push(frn);
                        } else {
                            tries.remove(&frn);
                        }
                    }
                }
            }
            if since.elapsed().as_secs() >= 600 {
                log::line(&format!("{}: {changes} journal records in the last {} s, {} names", self.letter, since.elapsed().as_secs(), table.read().map(|t| t.len()).unwrap_or(0)));
                changes = 0;
                since = Instant::now();
            }
        }
    }
}
