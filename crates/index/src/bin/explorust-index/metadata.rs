//! NTFS last write times. The USN journal timestamp dates the event, so it cannot rank files
//! by modification time. Read the MFT's basic information in bulk and refresh changed files by id.

use crate::hydration::{BATCH, Hydration};
use crate::log;
use crate::volume::{Volume, file_by_id, full_reference, ioctl};
use std::os::windows::io::AsRawHandle;
use std::time::Instant;
use windows::Win32::Foundation::{ERROR_HANDLE_EOF, ERROR_INVALID_DATA, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx};
use windows::Win32::System::Ioctl::{
    FILE_LAYOUT_ENTRY, FILE_LAYOUT_INFO_ENTRY_0, FSCTL_QUERY_FILE_LAYOUT, QUERY_FILE_LAYOUT_INCLUDE_EXTRA_INFO,
    QUERY_FILE_LAYOUT_INPUT, QUERY_FILE_LAYOUT_OUTPUT, QUERY_FILE_LAYOUT_RESTART,
};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
use windows_core::HSTRING;

pub fn last_write(file: HANDLE) -> Option<i64> {
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: writes the requested structure into a live local.
    unsafe { GetFileInformationByHandleEx(file, FileBasicInfo, (&mut info as *mut FILE_BASIC_INFO).cast(), size_of::<FILE_BASIC_INFO>() as u32) }.ok()?;
    Some(info.LastWriteTime)
}

pub fn last_write_by_id(volume: HANDLE, frn: u64) -> Option<i64> {
    let file = file_by_id(volume, frn)?;
    last_write(HANDLE(file.as_raw_handle()))
}

pub fn spawn(letter: char, path: HSTRING, sequences: Vec<u16>, hydration: Hydration) {
    let worker = std::thread::Builder::new().name(format!("index-{letter}-dates")).spawn(move || {
        // SAFETY: lowers only this worker's CPU and I/O priorities.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        // A separate file object lets these synchronous requests run while the journal waits.
        match Volume::open_path(letter, path) {
            Ok(volume) => hydrate(volume, &sequences, &hydration),
            Err(e) => log::line(&format!("{letter}: cannot open volume to hydrate last write times: {e}")),
        }
    });
    if let Err(e) = worker {
        log::line(&format!("{letter}: cannot start last write time worker: {e}"));
    }
}

fn hydrate(volume: Volume, sequences: &[u16], hydration: &Hydration) {
    let clock = Instant::now();
    let letter = volume.letter;
    let handle = volume.handle();
    let mut modified = 0;
    if let Err(e) = file_layout(handle, sequences, hydration, &mut modified) {
        log::line(&format!("{letter}: file layout unavailable ({e}), reading remaining last write times by ID"));
    }

    let mut dates = Vec::with_capacity(BATCH);
    for start in (0..sequences.len()).step_by(BATCH) {
        let end = (start + BATCH).min(sequences.len());
        let Some(pending) = hydration.pending(start..end) else { return };
        dates.clear();
        dates.extend(pending.into_iter().filter_map(|rec| {
            let frn = full_reference(rec, sequences[rec as usize]);
            last_write_by_id(handle, frn).map(|time| (rec, time))
        }));
        if !dates.is_empty() {
            let Some(n) = hydration.apply(&dates) else { return };
            modified += n;
        }
    }
    log::line(&format!("{letter}: last write times hydrated for {modified} names in {} ms", clock.elapsed().as_millis()));
}

fn file_layout(volume: HANDLE, sequences: &[u16], hydration: &Hydration, modified: &mut usize) -> windows_core::Result<()> {
    let mut input = QUERY_FILE_LAYOUT_INPUT { Flags: QUERY_FILE_LAYOUT_INCLUDE_EXTRA_INFO | QUERY_FILE_LAYOUT_RESTART, ..Default::default() };
    let mut buffer = vec![0u8; 1 << 20];
    let mut dates = Vec::new();
    while hydration.is_current() {
        let got = match ioctl(volume, FSCTL_QUERY_FILE_LAYOUT, &input, &mut buffer) {
            Ok(n) => n,
            Err(e) if [ERROR_HANDLE_EOF.to_hresult(), ERROR_NO_MORE_FILES.to_hresult()].contains(&e.code()) => return Ok(()),
            Err(e) => return Err(e),
        };
        input.Flags &= !QUERY_FILE_LAYOUT_RESTART;
        let bytes = buffer.get(..got).ok_or_else(invalid_layout)?;
        let output = header::<QUERY_FILE_LAYOUT_OUTPUT>(bytes, 0).ok_or_else(invalid_layout)?;
        if output.FileEntryCount == 0 {
            return Ok(());
        }
        let mut offset = output.FirstFileOffset as usize;
        if offset < size_of::<QUERY_FILE_LAYOUT_OUTPUT>() {
            return Err(invalid_layout());
        }
        dates.clear();
        for i in 0..output.FileEntryCount {
            let entry = header::<FILE_LAYOUT_ENTRY>(bytes, offset).ok_or_else(invalid_layout)?;
            let frn = entry.FileReferenceNumber;
            let rec = frn & fsindex::usn::MASK48;
            if entry.ExtraInfoOffset != 0 && sequences.get(rec as usize) == Some(&((frn >> 48) as u16)) {
                let at = offset + entry.ExtraInfoOffset as usize;
                let info = header::<FILE_LAYOUT_INFO_ENTRY_0>(bytes, at).ok_or_else(invalid_layout)?;
                dates.push((rec as u32, info.LastWriteTime));
            }
            if i + 1 < output.FileEntryCount && entry.NextFileOffset < size_of::<FILE_LAYOUT_ENTRY>() as u32 {
                return Err(invalid_layout());
            }
            offset += entry.NextFileOffset as usize;
        }
        for batch in dates.chunks(BATCH) {
            let Some(n) = hydration.apply(batch) else { return Ok(()) };
            *modified += n;
        }
    }
    Ok(())
}

fn header<T: Copy>(bytes: &[u8], offset: usize) -> Option<T> {
    let data = bytes.get(offset..offset.checked_add(size_of::<T>())?)?;
    // SAFETY: these FSCTL structs contain only integers and the entire struct is in the buffer.
    Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast()) })
}

fn invalid_layout() -> windows_core::Error {
    windows_core::Error::new(ERROR_INVALID_DATA.to_hresult(), "incomplete NTFS file layout")
}
