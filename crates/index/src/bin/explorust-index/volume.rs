//! One NTFS volume: its handle, the USN journal, the MFT enumeration and the journal follower.

use crate::retry::LinkRetry;
use crate::{log, metadata};
use fsindex::table::{RecState, Table};
use fsindex::usn::{self, Record};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::RwLock;
use std::time::Instant;
use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE, ERROR_MORE_DATA, GENERIC_READ, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA, FSCTL_QUERY_FILE_LAYOUT, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    MFT_ENUM_DATA_V0, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows_core::HSTRING;

const DRIVE_FIXED: u32 = 3;
/// Metadata files ($MFT, $LogFile, the root itself) sit below record 16.
const FIRST_USER_RECORD: u64 = 16;

fn full_reference(rec: u32, seq: u16) -> u64 {
    u64::from(rec) | (u64::from(seq) << 48)
}

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

// SAFETY: each Volume owns its handle. The link worker receives a duplicate and only uses it
// to open files by ID while the follower reads the journal through the original handle.
unsafe impl Send for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        // SAFETY: closes the handle opened in `open`.
        let _ = unsafe { CloseHandle(self.h) };
    }
}

pub(crate) fn ioctl<I>(h: HANDLE, code: u32, input: &I, out: &mut [u8]) -> windows_core::Result<usize> {
    let mut got = 0u32;
    // SAFETY: the input struct and the output buffer outlive the synchronous call.
    let result = unsafe {
        DeviceIoControl(
            h,
            code,
            Some(input as *const I as *const _),
            size_of::<I>() as u32,
            Some(out.as_mut_ptr().cast()),
            out.len() as u32,
            Some(&mut got),
            None,
        )
    };
    if let Err(e) = result
        && !(code == FSCTL_QUERY_FILE_LAYOUT && got != 0 && e.code() == ERROR_MORE_DATA.to_hresult())
    {
        return Err(e);
    }
    Ok(got as usize)
}

/// FileHardLinkInformation (FILE_INFORMATION_CLASS 46): every name of a file with its folder.
const FILE_HARD_LINK_INFORMATION: u32 = 46;

windows_core::link!("ntdll.dll" "system" fn NtQueryInformationFile(file: HANDLE, iosb: *mut [usize; 2], info: *mut core::ffi::c_void, len: u32, class: u32) -> i32);

pub(crate) fn file_by_id(volume: HANDLE, frn: u64) -> Option<OwnedHandle> {
    let id = FILE_ID_DESCRIPTOR { dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32, Type: FileIdType, Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 } };
    // SAFETY: opens by id for attributes only and hands ownership to OwnedHandle.
    let h = unsafe {
        OpenFileById(volume, &id, FILE_READ_ATTRIBUTES.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, None, FILE_FLAG_BACKUP_SEMANTICS).ok()?
    };
    Some(unsafe { OwnedHandle::from_raw_handle(h.0) })
}

struct LinkInfo {
    names: Vec<(u32, String)>,
    modified: i64,
}

/// The names of file `frn` (full reference, sequence included) as (parent record, name).
fn hard_links(volume: HANDLE, frn: u64, buf: &mut Vec<u8>) -> Option<LinkInfo> {
    let file = file_by_id(volume, frn)?;
    let h = HANDLE(file.as_raw_handle());
    let mut iosb = [0usize; 2];
    let mut st = 0;
    for _ in 0..2 {
        buf[..4].fill(0);
        // SAFETY: the buffer outlives the call; the class fills FILE_LINKS_INFORMATION.
        st = unsafe { NtQueryInformationFile(h, &mut iosb, buf.as_mut_ptr().cast(), buf.len() as u32, FILE_HARD_LINK_INFORMATION) };
        // Too many links for the buffer: NTFS reports the size it needs in BytesNeeded.
        let needed = buf.get(..4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        if st >= 0 || needed <= buf.len() || needed > 1 << 20 {
            break;
        }
        buf.resize(needed, 0);
    }
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
        let parent = u64::from_le_bytes(buf.get(o + 8..o + 16)?.try_into().ok()?) & usn::MASK48;
        let len = u32_at(o + 16)? as usize;
        let units: Vec<u16> = buf.get(o + 20..o + 20 + len * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        out.push((parent.min(u32::MAX as u64) as u32, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        o += next;
    }
    Some(LinkInfo { names: out, modified: metadata::last_write(h).unwrap_or(0) })
}

/// Reconciles the one alias we keep for a file. The other names may have disappeared.
fn set_link(table: &mut Table, rec: u32, info: LinkInfo) -> bool {
    let Some((parent, name)) = info.names.into_iter().find(|(p, _)| (*p == fsindex::table::ROOT || table.has_name(*p)) && !table.pruned_parent(*p)) else {
        table.prune(rec);
        return false;
    };
    if matches!(table.state(rec), RecState::Named(p, n) if p == parent && n == name) {
        table.set_modified(rec, info.modified);
        return false;
    }
    table.upsert(rec, parent, &name, false, false, info.modified);
    true
}

/// Finds outside aliases for files pruned by the initial MFT read, such as System32 binaries with
/// a WinSxS name. The journal may run during the lookup. Only write results to the original table
/// and to records that are still pruned and were not touched during the lookup.
/// Returns the counts of files looked at, names added and lookups that failed.
pub fn add_links(volume: HANDLE, frns: &[u64], table: &RwLock<Table>, id: u64) -> (usize, usize, usize) {
    let mut buf = vec![0u8; 8 << 10];
    let (mut looked, mut added, mut failed) = (0, 0, 0);
    {
        let mut t = table.write().unwrap_or_else(|p| p.into_inner());
        if t.id() != id {
            return (0, 0, failed);
        }
        t.watch_touches(true);
    }
    for chunk in frns.chunks(256) {
        let (pruned, since) = {
            let t = table.read().unwrap_or_else(|p| p.into_inner());
            if t.id() != id {
                break;
            }
            (chunk.iter().copied().filter(|&f| t.state((f & usn::MASK48) as u32) == RecState::Pruned).collect::<Vec<_>>(), t.touches())
        };
        let mut found = Vec::new();
        for frn in pruned {
            looked += 1;
            match hard_links(volume, frn, &mut buf) {
                Some(links) => found.push(((frn & usn::MASK48) as u32, links)),
                None => failed += 1,
            }
        }
        if found.is_empty() {
            continue;
        }
        let mut t = table.write().unwrap_or_else(|p| p.into_inner());
        if t.id() != id {
            break;
        }
        for (rec, links) in found {
            if t.state(rec) == RecState::Pruned && !t.touched_since(rec, since) {
                added += usize::from(set_link(&mut t, rec, links));
            }
        }
    }
    let mut t = table.write().unwrap_or_else(|p| p.into_inner());
    if t.id() == id {
        t.watch_touches(false);
    }
    (looked, added, failed)
}

fn apply(t: &mut Table, r: &Record, modified: i64) {
    if r.rec < FIRST_USER_RECORD || r.rec > u32::MAX as u64 - 1 {
        return;
    }
    t.touch(r.rec as u32);
    if r.reason & usn::REASON_FILE_DELETE != 0 {
        t.remove(r.rec as u32);
    } else if r.reason & usn::REASON_RENAME_OLD_NAME != 0 {
        // A close record can carry both the old-name reason and data changes.
        t.set_modified(r.rec as u32, modified);
    } else {
        let dir = r.attrs & usn::ATTR_DIRECTORY != 0;
        let noisy = r.attrs & (usn::ATTR_HIDDEN | usn::ATTR_SYSTEM) != 0;
        let parent = r.parent.min(u32::MAX as u64) as u32;
        t.upsert(r.rec as u32, parent, &r.name, dir, noisy, modified);
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

    pub fn duplicate(&self) -> windows_core::Result<Self> {
        let mut h = HANDLE::default();
        // SAFETY: both process handles refer to this process; the duplicate is owned by Self.
        unsafe {
            let process = GetCurrentProcess();
            DuplicateHandle(process, self.h, process, &mut h, 0, false, DUPLICATE_SAME_ACCESS)?;
        }
        Ok(Self { letter: self.letter, h })
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
                apply(&mut t, &r, 0)
            });
        }
        let pruned = t.settle();
        let clock = Instant::now();
        if let Err(e) = metadata::initial(self.h, &mut t, &seqs) {
            log::line(&format!("{}: file layout unavailable ({e}), reading remaining last write times by ID", self.letter));
        }
        let mut modified = 0;
        for (rec, &seq) in seqs.iter().enumerate() {
            let rec = rec as u32;
            if !t.has_name(rec) {
                continue;
            }
            if t.needs_modified(rec) && let Some(m) = metadata::last_write_by_id(self.h, full_reference(rec, seq)) {
                t.set_modified(rec, m);
            }
            modified += usize::from(!t.needs_modified(rec));
        }
        log::line(&format!("{}: last write times loaded for {modified}/{} names in {} ms", self.letter, t.len(), clock.elapsed().as_millis()));
        t.shrink();
        let files: Vec<u64> = t.take_pruned_files().into_iter().filter_map(|r| seqs.get(r as usize).map(|&seq| full_reference(r, seq))).collect();
        log::line(&format!("{}: {pruned} names inside WinSxS left out ({} files)", self.letter, files.len()));
        Ok((t, files))
    }

    fn changes(&self, bytes: &[u8], table: &RwLock<Table>, retries: &mut HashMap<u32, LinkRetry>) -> u64 {
        let mut records = Vec::new();
        let mut dates = HashMap::new();
        usn::parse(bytes, |r| {
            if !(FIRST_USER_RECORD..u32::MAX as u64).contains(&r.rec) {
                return;
            }
            let frn = full_reference(r.rec as u32, r.seq);
            if r.reason & (usn::REASON_FILE_CREATE | usn::REASON_BASIC_INFO_CHANGE | usn::REASON_DATA_CHANGE | usn::REASON_RENAME_NEW_NAME) != 0
                && r.reason & usn::REASON_FILE_DELETE == 0
            {
                dates.entry(frn).or_insert_with(|| metadata::last_write_by_id(self.h, frn).unwrap_or(0));
            }
            records.push(r);
        });
        let now = Instant::now();
        let mut t = table.write().unwrap_or_else(|p| p.into_inner());
        let mut refs = HashMap::new();
        for r in &records {
            let rec = r.rec as u32;
            let frn = full_reference(rec, r.seq);
            let deleted = r.reason & usn::REASON_FILE_DELETE != 0;
            let renamed_outside = r.reason & usn::REASON_RENAME_NEW_NAME != 0
                && r.reason & usn::REASON_HARD_LINK_CHANGE == 0
                && !t.pruned_parent(r.parent.min(u32::MAX as u64) as u32);
            if deleted || renamed_outside || retries.get(&rec).is_some_and(|job| job.frn != frn) {
                retries.remove(&rec);
            }
            apply(&mut t, r, dates.get(&frn).copied().unwrap_or(0));
            let current = refs.entry(rec).or_insert((frn, true));
            // A lookup queued earlier in this batch must not invalidate a later, known name.
            if deleted || renamed_outside {
                *current = (frn, false);
            } else if current.0 != frn || r.reason & (usn::REASON_FILE_CREATE | usn::REASON_HARD_LINK_CHANGE | usn::REASON_RENAME_NEW_NAME) != 0 {
                *current = (frn, true);
            }
        }
        for rec in t.take_pruned_files() {
            if let Some(&(frn, true)) = refs.get(&rec) && t.state(rec) != RecState::Free {
                retries.entry(rec).or_insert_with(|| LinkRetry::new(frn, now));
            }
        }
        records.len() as u64
    }

    fn retry_links(&self, table: &RwLock<Table>, retries: &mut HashMap<u32, LinkRetry>, buf: &mut Vec<u8>) {
        let now = Instant::now();
        let due: Vec<_> = retries.extract_if(|_, job| job.due <= now).collect();
        for (rec, job) in due {
            if job.attempts != 0 {
                log::line(&format!("{}: hard link retry timer: record {rec}, attempt {}", self.letter, job.attempts + 1));
            }
            if let Some(info) = hard_links(self.h, job.frn, buf) {
                let mut t = table.write().unwrap_or_else(|p| p.into_inner());
                t.touch(rec);
                set_link(&mut t, rec, info);
            } else if let Some(next) = job.after_failure(Instant::now()) {
                retries.insert(rec, next);
            } else {
                let mut t = table.write().unwrap_or_else(|p| p.into_inner());
                t.touch(rec);
                t.prune(rec);
                drop(t);
                log::line(&format!("{}: hard link retry exhausted: record {rec}, alias invalidated", self.letter));
            }
        }
    }

    /// Applies the journal from `next` on, blocking until changes come; returns on an error
    /// (journal deleted or wrapped past `next`), after which the caller reloads.
    pub fn follow(&self, journal_id: u64, mut next: i64, table: &RwLock<Table>) -> windows_core::Error {
        let mut buf = vec![0u8; 64 << 10];
        let mut changes = 0u64;
        let mut since = Instant::now();
        let mut retries = HashMap::new();
        let mut links_buf = vec![0u8; 8 << 10];
        loop {
            let rd = READ_USN_JOURNAL_DATA_V0 {
                StartUsn: next,
                ReasonMask: u32::MAX,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                // Only poll while a link retry has a deadline. With no pending retries, wait
                // indefinitely for USN records so an idle volume never wakes the follower.
                BytesToWaitFor: u64::from(retries.is_empty()),
                UsnJournalID: journal_id,
            };
            let got = match ioctl(self.h, FSCTL_READ_USN_JOURNAL, &rd, &mut buf) {
                Ok(n) => n,
                Err(e) => return e,
            };
            if got >= 8 {
                next = i64::from_le_bytes(buf[..8].try_into().unwrap_or_default());
            }
            if got > 8 {
                changes += self.changes(&buf[8..got], table, &mut retries);
            }
            if !retries.is_empty() {
                self.retry_links(table, &mut retries, &mut links_buf);
            }
            if got <= 8 && let Some(deadline) = retries.values().map(|r| r.due).min() {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            if since.elapsed().as_secs() >= 600 {
                log::line(&format!("{}: {changes} journal records in the last {} s, {} names", self.letter, since.elapsed().as_secs(), table.read().map(|t| t.len()).unwrap_or(0)));
                changes = 0;
                since = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsindex::table::ROOT;

    #[test]
    fn retry_timer_invalidates_an_unresolvable_alias() {
        let volume = Volume::open('C').expect("open NTFS test volume as administrator");
        let mut table = Table::new();
        let rec = 72;
        table.upsert(rec, ROOT, "obsolete.txt", false, false, 0);
        let table = RwLock::new(table);
        let now = Instant::now();
        // This reference is beyond NTFS's supported record range and cannot be opened.
        let mut retries = HashMap::from([(rec, LinkRetry::new(u64::MAX, now))]);
        let mut buf = vec![0; 8 << 10];

        volume.retry_links(&table, &mut retries, &mut buf);
        assert_eq!(table.read().unwrap().name(rec).as_deref(), Some("obsolete.txt"));
        for attempt in 1..=2 {
            let retry = retries.get(&rec).expect("a retry is scheduled even without a journal record");
            assert_eq!(retry.attempts, attempt);
            std::thread::sleep(retry.due.saturating_duration_since(Instant::now()));
            volume.retry_links(&table, &mut retries, &mut buf);
        }
        assert!(retries.is_empty());
        assert_eq!(table.read().unwrap().state(rec), RecState::Pruned);
    }
}
