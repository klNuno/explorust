//! One NTFS volume: its handle, the USN journal, the MFT enumeration and the journal follower.

use crate::log;
use fsindex::table::Table;
use fsindex::usn::{self, Record};
use std::sync::RwLock;
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING,
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

fn apply(t: &mut Table, r: &Record) {
    if r.rec < FIRST_USER_RECORD || r.rec > u32::MAX as u64 - 1 {
        return;
    }
    if r.reason & usn::REASON_FILE_DELETE != 0 {
        t.remove(r.rec as u32);
    } else if r.reason & usn::REASON_RENAME_OLD_NAME == 0 {
        let dir = r.attrs & usn::ATTR_DIRECTORY != 0;
        let noisy = r.attrs & (usn::ATTR_HIDDEN | usn::ATTR_SYSTEM) != 0;
        t.upsert(r.rec as u32, r.parent.min(u32::MAX as u64) as u32, &r.name, dir, noisy, r.usn);
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

    /// Every name in the MFT up to `high` into a new table.
    pub fn load(&self, high: i64) -> windows_core::Result<Table> {
        let mut t = Table::new();
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
            usn::parse(&buf[8..got], |r| apply(&mut t, &r));
        }
        t.settle();
        t.shrink();
        Ok(t)
    }

    /// Applies the journal from `next` on, blocking until changes come; returns on an error
    /// (journal deleted or wrapped past `next`), after which the caller reloads.
    pub fn follow(&self, journal_id: u64, mut next: i64, table: &RwLock<Table>) -> windows_core::Error {
        let mut buf = vec![0u8; 64 << 10];
        let mut changes = 0u64;
        let mut since = Instant::now();
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
                let mut t = table.write().unwrap_or_else(|p| p.into_inner());
                usn::parse(&buf[8..got], |r| {
                    changes += 1;
                    apply(&mut t, &r)
                });
            }
            if since.elapsed().as_secs() >= 600 {
                log::line(&format!("{}: {changes} journal records in the last {} s, {} names", self.letter, since.elapsed().as_secs(), table.read().map(|t| t.len()).unwrap_or(0)));
                changes = 0;
                since = Instant::now();
            }
        }
    }
}
