//! One NTFS volume: its handle, the USN journal, the MFT enumeration and the journal follower.

use crate::retry::{self, BackoffLog, LinkRetry};
use crate::{log, metadata};
use fsindex::table::{RecState, Snapshot, Table, reconcile};
use fsindex::usn::{self, Record};
use parking_lot::{RwLock, RwLockWriteGuard};
use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::time::Instant;
use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_HANDLE_EOF, ERROR_INVALID_DATA, ERROR_JOURNAL_NOT_ACTIVE, ERROR_MORE_DATA, GENERIC_READ, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, OPEN_EXISTING, OpenFileById,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA,
    FSCTL_QUERY_FILE_LAYOUT, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, MFT_ENUM_DATA_V0,
    NTFS_VOLUME_DATA_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows_core::HSTRING;

const DRIVE_FIXED: u32 = 3;
/// Metadata files ($MFT, $LogFile, the root itself) sit below record 16.
const FIRST_USER_RECORD: u64 = 16;

pub(crate) fn full_reference(rec: u32, seq: u16) -> u64 {
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
    path: HSTRING,
}

pub struct Mft {
    pub names: Table,
    pub sequences: Vec<u16>,
    pub pruned: Vec<u64>,
}

pub struct JournalPosition {
    next: i64,
    retries: HashMap<u32, LinkRetry>,
    changes: u64,
}

impl JournalPosition {
    pub fn new(next: i64) -> Self {
        Self { next, retries: HashMap::new(), changes: 0 }
    }

    pub fn changes(&self) -> u64 {
        self.changes
    }

    pub fn next(&self) -> i64 {
        self.next
    }

    pub fn clear_link_retries(&mut self, table: &RwLock<Table>) {
        let mut names = table.write();
        for (rec, _) in self.retries.drain() {
            names.finish_link_lookup(rec);
        }
    }
}

struct Replay<'a> {
    sequences: &'a mut Vec<u16>,
    resettle: bool,
}

// SAFETY: each Volume owns its handle. The workers receive their own handles.
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
    let buf = buf.get(..iosb[1])?;
    let u32_at = |o: usize| buf.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let n = u32_at(4)? as usize;
    if n == 0 || n > (buf.len() - 8) / 20 {
        return None;
    }
    let (mut o, mut out) = (8usize, Vec::with_capacity(n));
    for i in 0..n {
        let next = u32_at(o)? as usize;
        let parent = u64::from_le_bytes(buf.get(o + 8..o + 16)?.try_into().ok()?) & usn::MASK48;
        let len = u32_at(o + 16)? as usize;
        let size = 20 + len * 2;
        if len == 0 || (i + 1 < n && next < size) {
            return None;
        }
        let units: Vec<u16> = buf.get(o + 20..o + size)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        out.push((u32::try_from(parent).ok()?, String::from_utf16_lossy(&units)));
        o += next;
    }
    Some(LinkInfo { names: out, modified: metadata::last_write(h).unwrap_or(0) })
}

/// Reconciles the one alias we keep for a file. The other names may have disappeared.
fn set_link(table: &mut Table, rec: u32, info: &LinkInfo) -> bool {
    let Some((parent, name)) = info.names.iter().find(|(p, _)| (*p == fsindex::table::ROOT || table.has_name(*p)) && !table.pruned_parent(*p)) else {
        table.prune(rec);
        return false;
    };
    if matches!(table.state(rec), RecState::Named(p, n) if p == *parent && n == *name) {
        table.set_modified(rec, info.modified);
        return false;
    }
    table.upsert(rec, *parent, name, false, false, info.modified);
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
        let mut t = table.write();
        if t.id() != id {
            return (0, 0, failed);
        }
        t.watch_touches(true);
    }
    for chunk in frns.chunks(256) {
        let (pruned, since) = {
            let t = table.read();
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
        let mut t = table.write();
        if t.id() != id {
            break;
        }
        for (rec, links) in found {
            if t.state(rec) == RecState::Pruned && !t.touched_since(rec, since) {
                added += usize::from(set_link(&mut t, rec, &links));
                t.set_aliases(rec, links.names);
            }
        }
    }
    let mut t = table.write();
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
    let dir = r.attrs & usn::ATTR_DIRECTORY != 0;
    if r.reason & usn::REASON_FILE_DELETE != 0 {
        t.remove(r.rec as u32);
        return;
    } else if r.reason & usn::REASON_RENAME_OLD_NAME != 0 {
        // A close record can carry both the old-name reason and data changes.
        t.set_modified(r.rec as u32, modified);
    } else {
        let noisy = r.attrs & (usn::ATTR_HIDDEN | usn::ATTR_SYSTEM) != 0;
        let parent = r.parent.min(u32::MAX as u64) as u32;
        t.upsert(r.rec as u32, parent, &r.name, dir, noisy, modified);
    }
    // A rename or hard-link change can move an alias into a previously covered scope.
    if !dir && r.reason & (usn::REASON_HARD_LINK_CHANGE | usn::REASON_RENAME_NEW_NAME) != 0 {
        t.recheck_links(r.rec as u32);
    }
}

impl Volume {
    pub fn open(letter: char) -> windows_core::Result<Volume> {
        let root = HSTRING::from(format!("{letter}:\\"));
        let mut name = [0u16; 64];
        // SAFETY: writes into the given volume GUID path buffer.
        unsafe { GetVolumeNameForVolumeMountPointW(&root, &mut name)? };
        let name = name.split(|&c| c == 0).next().unwrap_or_default();
        // A trailing backslash opens the root directory, rather than the volume device.
        let path = HSTRING::from_wide(name.strip_suffix(&[b'\\' as u16]).unwrap_or(name));
        Self::open_path(letter, path)
    }

    pub fn open_path(letter: char, path: HSTRING) -> windows_core::Result<Self> {
        // SAFETY: opens the volume for reads; the handle is closed on drop.
        let h = unsafe {
            CreateFileW(&path, GENERIC_READ.0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)?
        };
        Ok(Self { letter, h, path })
    }

    /// A new handle opened by this path accesses the same volume even after a letter change.
    pub fn device_path(&self) -> HSTRING {
        self.path.clone()
    }

    pub fn duplicate(&self) -> windows_core::Result<Self> {
        let mut h = HANDLE::default();
        // SAFETY: both process handles refer to this process; the duplicate is owned by Self.
        unsafe {
            let process = GetCurrentProcess();
            DuplicateHandle(process, self.h, process, &mut h, 0, false, DUPLICATE_SAME_ACCESS)?;
        }
        Ok(Self { letter: self.letter, h, path: self.path.clone() })
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

    fn highest_mft_record(&self) -> Option<u64> {
        let mut buffer = [0; size_of::<NTFS_VOLUME_DATA_BUFFER>() + 64];
        let bytes = ioctl(self.h, FSCTL_GET_NTFS_VOLUME_DATA, &(), &mut buffer).ok()?;
        if bytes < size_of::<NTFS_VOLUME_DATA_BUFFER>() {
            return None;
        }
        // SAFETY: the ioctl filled at least the fixed prefix, which contains only integers.
        let data = unsafe { buffer.as_ptr().cast::<NTFS_VOLUME_DATA_BUFFER>().read_unaligned() };
        u64::try_from(data.MftValidDataLength)
            .ok()?
            .checked_div(u64::from(data.BytesPerFileRecordSegment))?
            .checked_sub(1)
    }

    /// Reads the names up to `high`, keeping the sequence numbers for later metadata lookups.
    pub fn load(&self, high: i64) -> windows_core::Result<Mft> {
        let mut t = Table::new();
        if let Some(highest) = self.highest_mft_record() {
            t.cover_mft_records(highest);
        }
        let mut seqs: Vec<u16> = Vec::new();
        self.enumerate_mft(high, |bytes| usn::parse_mft(bytes, |_| true, |r| {
            if let Some(i) = usize::try_from(r.rec).ok().filter(|&i| i < u32::MAX as usize) {
                if seqs.len() <= i {
                    seqs.resize(i + 1, 0);
                }
                seqs[i] = r.seq;
            }
            apply(&mut t, &r, 0)
        }))?;
        let pruned = t.settle();
        t.shrink();
        let files: Vec<u64> = t.take_pruned_files().into_iter().filter_map(|r| seqs.get(r as usize).map(|&seq| full_reference(r, seq))).collect();
        log::line(&format!("{}: {pruned} names inside WinSxS left out ({} files)", self.letter, files.len()));
        Ok(Mft { names: t, sequences: seqs, pruned: files })
    }

    fn enumerate_mft(&self, high: i64, mut batch: impl FnMut(&[u8]) -> bool) -> windows_core::Result<()> {
        let mut med = MFT_ENUM_DATA_V0 { StartFileReferenceNumber: 0, LowUsn: 0, HighUsn: high };
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let got = match ioctl(self.h, FSCTL_ENUM_USN_DATA, &med, &mut buf) {
                Ok(n) => n,
                Err(e) if e.code() == ERROR_HANDLE_EOF.to_hresult() => break,
                Err(e) => return Err(e),
            };
            let next = u64::from_le_bytes(buf[..8].try_into().unwrap());
            if got < 8 {
                return Err(ERROR_INVALID_DATA.into());
            }
            if next & usn::MASK48 <= med.StartFileReferenceNumber & usn::MASK48 {
                if got == 8 {
                    break;
                }
                return Err(ERROR_INVALID_DATA.into());
            }
            if !batch(&buf[8..got]) {
                return Err(ERROR_INVALID_DATA.into());
            }
            med.StartFileReferenceNumber = next;
        }
        Ok(())
    }

    pub fn reconcile(&self, table: &RwLock<Table>, sequences: &mut Vec<u16>, pruned: &mut Vec<u64>) -> windows_core::Result<()> {
        if let Some(highest) = self.highest_mft_record() {
            table.write().cover_mft_records(highest);
        }
        let mut records = Vec::new();
        reconcile(table, sequences, pruned, |dirs, apply| self.reconcile_pass(dirs, &mut records, apply))
    }

    fn reconcile_pass(&self, dirs: bool, records: &mut Vec<Record>, apply: &mut dyn FnMut(&[Record])) -> windows_core::Result<()> {
        self.enumerate_mft(i64::MAX, |bytes| {
            records.clear();
            if !usn::parse_mft(bytes, |attrs| (attrs & usn::ATTR_DIRECTORY != 0) == dirs, |record| records.push(record)) {
                return false;
            }
            apply(records);
            true
        })
    }

    /// Apply the closed interval since the snapshot before publishing any of its names.
    /// The live follower keeps the same retry jobs and resumes at the returned cursor.
    pub fn replay(&self, snapshot: Snapshot) -> windows_core::Result<(Mft, JournalPosition)> {
        let Snapshot { names, cursor, mut sequences, mut pruned } = snapshot;
        let journal = self.journal()?;
        let low = journal.FirstUsn.max(journal.LowestValidUsn);
        if cursor.journal != journal.UsnJournalID || !(low..=journal.NextUsn).contains(&cursor.next) {
            return Err(ERROR_INVALID_DATA.into());
        }
        let mut position = JournalPosition::new(cursor.next);
        let table = RwLock::new(names);
        let mut replay = Replay { sequences: &mut sequences, resettle: false };
        let mut buf = vec![0u8; 64 << 10];
        while position.next < journal.NextUsn {
            let rd = READ_USN_JOURNAL_DATA_V0 {
                StartUsn: position.next, ReasonMask: u32::MAX, ReturnOnlyOnClose: 0,
                Timeout: 0, BytesToWaitFor: 0, UsnJournalID: cursor.journal,
            };
            let got = ioctl(self.h, FSCTL_READ_USN_JOURNAL, &rd, &mut buf)?;
            let bytes = buf.get(..got).filter(|bytes| bytes.len() >= 8)
                .ok_or_else(|| windows_core::Error::from(ERROR_INVALID_DATA))?;
            let next = i64::from_le_bytes(bytes[..8].try_into().unwrap());
            if next <= position.next {
                return Err(ERROR_INVALID_DATA.into());
            }
            position.changes += self.changes(&bytes[8..], &table, &mut position.retries, Some(&mut replay))?;
            position.next = next;
        }
        let mut names = table.into_inner();
        if replay.resettle {
            names.settle();
            pruned.extend(names.take_pruned_files().into_iter().filter_map(|rec| {
                sequences.get(rec as usize).map(|&seq| full_reference(rec, seq))
            }));
        }
        // A cached pruned record may have been deleted and reused while the shell was off.
        pruned.retain(|&frn| sequences.get((frn & usn::MASK48) as usize) == Some(&((frn >> 48) as u16)));
        Ok((Mft { names, sequences, pruned }, position))
    }

    fn changes(
        &self, bytes: &[u8], table: &RwLock<Table>, retries: &mut HashMap<u32, LinkRetry>,
        mut replay: Option<&mut Replay<'_>>,
    ) -> windows_core::Result<u64> {
        let mut records = Vec::new();
        let mut dates = HashMap::new();
        usn::parse(bytes, |r| {
            if !(FIRST_USER_RECORD..u32::MAX as u64).contains(&r.rec) {
                return;
            }
            let frn = full_reference(r.rec as u32, r.seq);
            // The initial layout pass already gets the latest timestamps after a restore.
            if replay.is_none()
                && r.reason & (usn::REASON_FILE_CREATE | usn::REASON_BASIC_INFO_CHANGE | usn::REASON_DATA_CHANGE | usn::REASON_RENAME_NEW_NAME) != 0
                && r.reason & usn::REASON_FILE_DELETE == 0
            {
                dates.entry(frn).or_insert_with(|| metadata::last_write_by_id(self.h, frn).unwrap_or(0));
            }
            records.push(r);
        });
        let now = Instant::now();
        let mut t = table.write();
        let mut refs = HashMap::new();
        for r in &records {
            let rec = r.rec as u32;
            let parent = r.parent.min(u32::MAX as u64) as u32;
            let frn = full_reference(rec, r.seq);
            let deleted = r.reason & usn::REASON_FILE_DELETE != 0;
            let directory = r.attrs & usn::ATTR_DIRECTORY != 0;
            let moved = r.reason & (usn::REASON_FILE_DELETE | usn::REASON_RENAME_NEW_NAME) != 0;
            // A pruned directory moved into view has descendants whose names we never kept.
            if directory && moved && t.pruned_parent(rec) && !t.pruned_parent(parent) {
                return Err(ERROR_INVALID_DATA.into());
            }
            if let Some(replay) = replay.as_deref_mut() {
                replay.resettle |= directory;
                let slot = rec as usize;
                if replay.sequences.len() <= slot {
                    replay.sequences.resize(slot + 1, 0);
                }
                replay.sequences[slot] = if deleted { 0 } else { r.seq };
            }
            let repair_name = r.reason & usn::REASON_HARD_LINK_CHANGE != 0
                || t.pruned_parent(parent)
                || r.reason & (usn::REASON_RENAME_NEW_NAME | usn::REASON_RENAME_OLD_NAME)
                    == (usn::REASON_RENAME_NEW_NAME | usn::REASON_RENAME_OLD_NAME);
            if deleted || retries.get(&rec).is_some_and(|job| job.frn != frn) {
                retries.remove(&rec);
                t.finish_link_lookup(rec);
            }
            apply(&mut t, r, dates.get(&frn).copied().unwrap_or(0));
            if deleted {
                refs.remove(&rec);
            } else {
                let current = refs.entry(rec).or_insert((frn, false));
                // A rename giving us a new name can still follow a hard-link change in this batch.
                *current = (frn, repair_name || (current.0 == frn && current.1));
            }
        }
        for rec in t.take_pruned_files() {
            if let Some(&(frn, repair_name)) = refs.get(&rec) && t.state(rec) != RecState::Free {
                t.require_link_lookup(rec);
                retry::queue(retries, rec, frn, now, repair_name);
            }
        }
        Ok(records.len() as u64)
    }

    fn retry_links(&self, table: &RwLock<Table>, retries: &mut HashMap<u32, LinkRetry>, mut lookup: impl FnMut(u64) -> Option<LinkInfo>) -> usize {
        let now = Instant::now();
        let due: Vec<_> = retries.extract_if(|_, job| job.due <= now).collect();
        let mut backed_off = 0;
        for (rec, job) in due {
            if let Some(info) = lookup(job.frn) {
                let mut t = table.write();
                t.touch(rec);
                if info.names.len() > 1 {
                    t.mark_alias_parents(info.names.iter().map(|&(parent, _)| parent));
                }
                if job.repair_name {
                    set_link(&mut t, rec, &info);
                } else {
                    t.set_modified(rec, info.modified);
                }
                t.set_aliases(rec, info.names);
                t.finish_link_lookup(rec);
            } else {
                let next = job.after_failure(Instant::now());
                if next.recovering() && !job.recovering() {
                    if job.repair_name {
                        let mut t = table.write();
                        t.touch(rec);
                        t.prune(rec);
                    }
                    backed_off += 1;
                }
                retries.insert(rec, next);
            }
        }
        backed_off
    }

    /// Applies the journal from `next` on, blocking until changes come; returns on an error
    /// (journal deleted or wrapped past `next`), after which the caller reloads.
    pub fn follow(&self, journal_id: u64, position: JournalPosition, table: &RwLock<Table>) -> windows_core::Error {
        let JournalPosition { mut next, mut retries, mut changes } = position;
        let mut buf = vec![0u8; 64 << 10];
        let mut since = Instant::now();
        let mut backoff = BackoffLog::default();
        let mut links_buf = vec![0u8; 8 << 10];
        let mut caught_up = false;
        loop {
            let rd = READ_USN_JOURNAL_DATA_V0 {
                StartUsn: next,
                ReasonMask: u32::MAX,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                // Only poll while a link retry has a deadline. With no pending retries, wait
                // indefinitely for USN records so an idle volume never wakes the follower.
                BytesToWaitFor: u64::from(caught_up && retries.is_empty()),
                UsnJournalID: journal_id,
            };
            let got = match ioctl(self.h, FSCTL_READ_USN_JOURNAL, &rd, &mut buf) {
                Ok(n) => n,
                Err(e) => {
                    table.write().journal_current = false;
                    return e;
                }
            };
            if got >= 8 {
                next = i64::from_le_bytes(buf[..8].try_into().unwrap_or_default());
            }
            if got > 8 {
                match self.changes(&buf[8..got], table, &mut retries, None) {
                    Ok(applied) => changes += applied,
                    Err(e) => {
                        table.write().journal_current = false;
                        return e;
                    }
                }
            } else if !caught_up {
                table.write().journal_current = true;
                caught_up = true;
            }
            let had_retries = !retries.is_empty();
            if had_retries {
                backoff.backed_off(self.retry_links(table, &mut retries, |frn| hard_links(self.h, frn, &mut links_buf)));
            }
            if let Some(count) = backoff.summary(Instant::now()) {
                log::line(&format!("{}: hard link lookups backing off: {count} entered slow retry, {} lookups pending", self.letter, retries.len()));
            }
            if got > 8 || had_retries {
                loop {
                    let mut names = table.write();
                    let more = names.compact_pending_slice();
                    // Pass the lock to any waiting reader before this writer can reacquire it.
                    RwLockWriteGuard::unlock_fair(names);
                    if !more {
                        break;
                    }
                }
            }
            if got <= 8 && let Some(deadline) = retries.values().map(|r| r.due).min() {
                // Keep following unrelated journal changes while a long retry is pending.
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()).min(retry::RETRY_INTERVAL));
            }
            if since.elapsed().as_secs() >= 600 {
                log::line(&format!("{}: {changes} journal records in the last {} s, {} names", self.letter, since.elapsed().as_secs(), table.read().len()));
                changes = 0;
                since = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsindex::table::{ROOT, SnapshotCursor};
    use std::fs::{self, File, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn file_reference(file: &File) -> u64 {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info).unwrap() };
        u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow)
    }

    fn event(frn: u64, parent: u32, reason: u32, name: &str) -> Vec<u8> {
        let mut bytes = usn::encode(frn & usn::MASK48, parent.into(), 100, reason, 0, name);
        bytes[8..16].copy_from_slice(&frn.to_le_bytes());
        bytes
    }

    #[test]
    fn restore_replays_creates_renames_and_deletes_and_rejects_a_different_journal() {
        let volume = Volume::open('C').expect("open NTFS test volume as administrator");
        let dir = std::env::temp_dir().join(format!("explorust-snapshot-replay-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let fixture = Fixture(dir);
        let rename = fixture.0.join("replay-before-name");
        let delete = fixture.0.join("replay-delete-name");
        let after = fixture.0.join("replay-renamed-name");
        let new = fixture.0.join("replay-created-name");
        fs::write(&rename, b"rename") .unwrap();
        fs::write(&delete, b"delete").unwrap();

        let journal = volume.journal().unwrap();
        let cursor = SnapshotCursor { journal: journal.UsnJournalID, next: journal.NextUsn, boot: [1; 16] };
        let mft = volume.load(cursor.next).unwrap();
        let mut bytes = Vec::new();
        Snapshot::write(&mut bytes, &mft.names, cursor, &mft.sequences, &mft.pruned).unwrap();
        drop(mft);
        let read = || Snapshot::read_if(bytes.as_slice(), bytes.len() as u64, |_| true).unwrap().unwrap();
        let mut old = read();
        old.cursor.journal ^= 1;
        assert!(volume.replay(old).is_err());

        fs::rename(&rename, &after).unwrap();
        fs::remove_file(&delete).unwrap();
        fs::write(&new, b"create").unwrap();
        let (loaded, journal) = volume.replay(read()).unwrap();
        assert!(journal.changes() >= 3);
        for expected in [&after, &new] {
            let query = expected.file_name().unwrap().to_str().unwrap();
            let hits = loaded.names.query(query, 10);
            assert!(hits.iter().any(|hit| loaded.names.path(hit.rec, "C:\\")
                .is_some_and(|path| path.eq_ignore_ascii_case(expected.to_str().unwrap()))));
        }
        for absent in [&rename, &delete] {
            let query = absent.file_name().unwrap().to_str().unwrap();
            assert!(loaded.names.query(query, 10).is_empty());
        }
    }

    #[test]
    fn mft_repairs_provisional_names_and_keeps_writes_during_the_scan() {
        let volume = Volume::open('C').unwrap();
        let dir = std::env::temp_dir().join(format!("explorust-mft-reconcile-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let fixture = Fixture(dir);
        let actual = fixture.0.join("current-mft-name.txt");
        let concurrent = fixture.0.join("concurrent-mft-name.txt");
        let created = fixture.0.join("created-during-mft.txt");
        let deleted = fixture.0.join("deleted-during-mft.txt");
        fs::write(&actual, b"actual").unwrap();
        fs::write(&deleted, b"delete").unwrap();
        let rec = file_reference(&File::open(&actual).unwrap()) as u32;
        let parent = file_reference(&OpenOptions::new().read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0).open(&fixture.0).unwrap()) as u32;

        let journal = volume.journal().unwrap();
        let boot = crate::cache::boot();
        assert_ne!(boot, [0; 16]);
        let mut prior_boot = boot;
        prior_boot[0] ^= 1;
        let mut cursor = SnapshotCursor { journal: journal.UsnJournalID, next: journal.NextUsn, boot: prior_boot };
        let Mft { mut names, mut sequences, mut pruned } = volume.load(cursor.next).unwrap();
        names.upsert(rec, parent, "offline-stale-name.txt", false, false, 0);
        names.compact();
        let mut previous = Vec::new();
        Snapshot::write(&mut previous, &names, cursor, &sequences, &pruned).unwrap();
        drop(names);
        let read = |bytes: &[u8]| Snapshot::read_if(bytes, bytes.len() as u64, |_| true).unwrap().unwrap();
        let provisional = read(&previous);
        assert!(!provisional.cursor.verified_in(boot));
        assert_eq!(provisional.names.name(rec).as_deref(), Some("offline-stale-name.txt"));

        let table = RwLock::new(provisional.names);
        let mut records = Vec::new();
        reconcile(&table, &mut sequences, &mut pruned, |dirs, apply| {
            volume.reconcile_pass(dirs, &mut records, apply)?;
            if !dirs {
                // These writes are after this part of the MFT was read, before marking the
                // snapshot verified. Only the saved USN cursor can recover them.
                fs::rename(&actual, &concurrent).unwrap();
                fs::write(&created, b"create").unwrap();
                fs::remove_file(&deleted).unwrap();
            }
            assert!(!read(&previous).cursor.verified_in(boot));
            Ok::<_, windows_core::Error>(())
        }).unwrap();
        let names = table.into_inner();
        assert_eq!(names.name(rec).as_deref(), Some("current-mft-name.txt"));
        assert!(!names.journal_current);

        cursor.boot = boot;
        let mut verified = Vec::new();
        Snapshot::write(&mut verified, &names, cursor, &sequences, &pruned).unwrap();
        drop(names);
        let snapshot = read(&verified);
        assert!(snapshot.cursor.verified_in(boot));
        let (updated, position) = volume.replay(snapshot).unwrap();
        assert!(position.changes() >= 3);
        assert_eq!(updated.names.name(rec).as_deref(), Some("concurrent-mft-name.txt"));
        assert_eq!(updated.names.query("created-during-mft.txt", 1).len(), 1);
        assert!(updated.names.query("deleted-during-mft.txt", 1).is_empty());
    }

    #[test]
    fn hard_link_change_repairs_the_name_after_a_failed_rename_lookup() {
        let volume = Volume::open('C').expect("open NTFS test volume as administrator");
        let dir = std::env::temp_dir().join(format!("explorust-link-repair-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let fixture = Fixture(dir);
        let first = fixture.0.join("First");
        let second = fixture.0.join("Second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let dir_reference = |path| file_reference(&OpenOptions::new().read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0).open(path).unwrap()) as u32;
        let (first_id, second_id) = (dir_reference(&first), dir_reference(&second));
        let before = first.join("before.txt");
        let removed = first.join("removed.txt");
        let survivor = second.join("survivor.txt");
        fs::write(&before, "the same NTFS record").unwrap();
        fs::hard_link(&before, &survivor).unwrap();
        let frn = file_reference(&File::open(&before).unwrap());
        let rec = u32::try_from(frn & usn::MASK48).unwrap();

        let mut names = Table::new();
        names.upsert(first_id, ROOT, "First", true, false, 0);
        names.upsert(second_id, ROOT, "Second", true, false, 0);
        names.upsert(rec, first_id, "before.txt", false, false, 0);
        names.journal_current = true;
        names.initial_links_complete = true;
        let table = RwLock::new(names);
        let mut retries = HashMap::new();

        fs::rename(&before, &removed).unwrap();
        let rename = event(frn, first_id, usn::REASON_RENAME_NEW_NAME, "removed.txt");
        volume.changes(&rename, &table, &mut retries, None).unwrap();
        assert!(!retries[&rec].repair_name);
        volume.retry_links(&table, &mut retries, |_| None);
        fs::remove_file(&removed).unwrap();
        let deletion = event(frn, first_id, usn::REASON_HARD_LINK_CHANGE, "removed.txt");
        volume.changes(&deletion, &table, &mut retries, None).unwrap();
        let mut buffer = vec![0; 8 << 10];
        volume.retry_links(&table, &mut retries, |frn| hard_links(volume.h, frn, &mut buffer));

        assert!(retries.is_empty());
        let names = table.read();
        let hits = names.query_scope("survivor", 10, "C:\\", "C:\\Second").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(names.path(hits[0].rec, "C:\\").as_deref(), Some("C:\\Second\\survivor.txt"));
        assert!(names.query("removed", 10).is_empty());
        println!("The native NTFS lookup returned the surviving alias after removal of the indexed link");
    }

    #[test]
    fn a_slow_retry_restores_scoped_coverage_without_another_journal_record() {
        let volume = Volume::open('C').expect("open NTFS test volume as administrator");
        let mut names = Table::new();
        let rec = 72;
        names.upsert(71, ROOT, "Other", true, false, 0);
        names.upsert(73, 71, "unrelated.txt", false, false, 0);
        names.upsert(rec, ROOT, "obsolete.txt", false, false, 0);
        names.journal_current = true;
        names.initial_links_complete = true;
        let table = RwLock::new(names);
        let mut retries = HashMap::new();
        let change = usn::encode(u64::from(rec), ROOT.into(), 100, usn::REASON_HARD_LINK_CHANGE, 0, "obsolete.txt");
        volume.changes(&change, &table, &mut retries, None).unwrap();

        for _ in 0..3 {
            retries.get_mut(&rec).unwrap().due = Instant::now();
            volume.retry_links(&table, &mut retries, |_| None);
        }
        assert!(retries[&rec].recovering());
        assert_eq!(table.read().state(rec), RecState::Pruned);
        assert!(table.read().query_scope("unrelated", 10, "C:\\", "C:\\Other").is_none());

        retries.get_mut(&rec).unwrap().due = Instant::now();
        volume.retry_links(&table, &mut retries, |_| Some(LinkInfo {
            names: vec![(ROOT, "survivor.txt".into())],
            modified: 1,
        }));
        assert!(retries.is_empty());
        let names = table.read();
        assert_eq!(names.query_scope("unrelated", 10, "C:\\", "C:\\Other").unwrap().len(), 1);
        assert_eq!(names.name(rec).as_deref(), Some("survivor.txt"));
    }
}
