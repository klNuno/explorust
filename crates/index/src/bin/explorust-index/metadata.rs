//! NTFS last write times. The USN journal timestamp dates the event, so it cannot rank files
//! by modification time. Read the MFT's basic information in bulk and refresh changed files by id.

use crate::hydration::{BATCH, Hydration};
use crate::log;
use crate::volume::{Volume, file_by_id, full_reference, ioctl};
use fsindex::usn::{ATTR_DIRECTORY, MASK48};
use std::collections::HashSet;
use std::ops::Range;
use std::os::windows::io::AsRawHandle;
use std::time::Instant;
use windows::Win32::Foundation::{ERROR_HANDLE_EOF, ERROR_INVALID_DATA, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx};
use windows::Win32::System::Ioctl::{
    FILE_LAYOUT_ENTRY, FILE_LAYOUT_INFO_ENTRY_0, FILE_LAYOUT_NAME_ENTRY, FILE_LAYOUT_NAME_ENTRY_DOS,
    FILE_LAYOUT_NAME_ENTRY_PRIMARY, FSCTL_QUERY_FILE_LAYOUT, QUERY_FILE_LAYOUT_INCLUDE_EXTRA_INFO,
    QUERY_FILE_LAYOUT_INCLUDE_NAMES, QUERY_FILE_LAYOUT_INPUT, QUERY_FILE_LAYOUT_OUTPUT, QUERY_FILE_LAYOUT_RESTART,
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
    match file_layout(handle, sequences, hydration, &mut modified) {
        Ok(parents) => {
            let count = parents.len();
            if hydration.finish_links(parents) {
                log::line(&format!("{letter}: scoped search checked hard links, {count} directories contain hard links"));
            }
        }
        Err(e) => log::line(&format!("{letter}: file layout unavailable ({e}), scoped coverage unavailable; reading remaining last write times by ID")),
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

fn file_layout(volume: HANDLE, sequences: &[u16], hydration: &Hydration, modified: &mut usize) -> windows_core::Result<HashSet<u32>> {
    let mut input = QUERY_FILE_LAYOUT_INPUT {
        Flags: QUERY_FILE_LAYOUT_INCLUDE_EXTRA_INFO | QUERY_FILE_LAYOUT_INCLUDE_NAMES | QUERY_FILE_LAYOUT_RESTART,
        ..Default::default()
    };
    let mut buffer = vec![0u8; 1 << 20];
    let mut dates = Vec::new();
    let mut names = Vec::new();
    let mut links = Vec::new();
    let mut alias_parents = HashSet::new();
    while hydration.is_current() {
        let got = match ioctl(volume, FSCTL_QUERY_FILE_LAYOUT, &input, &mut buffer) {
            Ok(n) => n,
            Err(e) if [ERROR_HANDLE_EOF.to_hresult(), ERROR_NO_MORE_FILES.to_hresult()].contains(&e.code()) => break,
            Err(e) => return Err(e),
        };
        input.Flags &= !QUERY_FILE_LAYOUT_RESTART;
        let bytes = buffer.get(..got).ok_or_else(invalid_layout)?;
        let output = header::<QUERY_FILE_LAYOUT_OUTPUT>(bytes, 0).ok_or_else(invalid_layout)?;
        if output.FileEntryCount == 0 {
            break;
        }
        let mut offset = output.FirstFileOffset as usize;
        if offset < size_of::<QUERY_FILE_LAYOUT_OUTPUT>() {
            return Err(invalid_layout());
        }
        dates.clear();
        links.clear();
        for i in 0..output.FileEntryCount {
            let entry = header::<FILE_LAYOUT_ENTRY>(bytes, offset).ok_or_else(invalid_layout)?;
            let frn = entry.FileReferenceNumber;
            let rec = frn & MASK48;
            if entry.ExtraInfoOffset != 0 && sequences.get(rec as usize) == Some(&((frn >> 48) as u16)) {
                let at = offset + entry.ExtraInfoOffset as usize;
                let info = header::<FILE_LAYOUT_INFO_ENTRY_0>(bytes, at).ok_or_else(invalid_layout)?;
                dates.push((rec as u32, info.LastWriteTime));
            }
            if i + 1 < output.FileEntryCount && entry.NextFileOffset < size_of::<FILE_LAYOUT_ENTRY>() as u32 {
                return Err(invalid_layout());
            }
            if entry.FileAttributes & ATTR_DIRECTORY == 0 && rec >= 16 {
                let end = if entry.NextFileOffset == 0 { bytes.len() } else { offset + entry.NextFileOffset as usize };
                let entry_bytes = bytes.get(offset..end).ok_or_else(invalid_layout)?;
                if !layout_names(entry_bytes, entry.FirstNameOffset as usize, &mut names) {
                    return Err(invalid_layout());
                }
                if names.len() > 1 {
                    alias_parents.extend(names.iter().map(|(parent, _)| *parent));
                    if sequences.get(rec as usize) == Some(&((frn >> 48) as u16)) {
                        let aliases = names.iter().map(|(parent, range)| {
                            let name: Vec<_> = entry_bytes[range.clone()].chunks_exact(2)
                                .map(|unit| u16::from_le_bytes([unit[0], unit[1]])).collect();
                            (*parent, String::from_utf16_lossy(&name))
                        }).collect();
                        links.push((rec as u32, aliases));
                    }
                }
            }
            offset += entry.NextFileOffset as usize;
        }
        if !hydration.apply_aliases(links.drain(..)) {
            break;
        }
        for batch in dates.chunks(BATCH) {
            let Some(n) = hydration.apply(batch) else { break };
            *modified += n;
        }
    }
    Ok(alias_parents)
}

fn layout_names(bytes: &[u8], mut offset: usize, names: &mut Vec<(u32, Range<usize>)>) -> bool {
    const NAME: usize = std::mem::offset_of!(FILE_LAYOUT_NAME_ENTRY, FileName);
    names.clear();
    while offset != 0 {
        let Some(prefix) = bytes.get(offset..offset + NAME) else { return false };
        let next = u32::from_le_bytes(prefix[..4].try_into().unwrap()) as usize;
        let flags = u32::from_le_bytes(prefix[4..8].try_into().unwrap());
        let parent = u64::from_le_bytes(prefix[8..16].try_into().unwrap()) & MASK48;
        let length = u32::from_le_bytes(prefix[16..20].try_into().unwrap()) as usize;
        if length == 0 || length % 2 != 0 || offset + NAME + length > bytes.len() {
            return false;
        }
        // A DOS-only 8.3 directory entry is the alternate spelling of a long name.
        if flags & FILE_LAYOUT_NAME_ENTRY_DOS == 0 || flags & FILE_LAYOUT_NAME_ENTRY_PRIMARY != 0 {
            let Ok(parent) = u32::try_from(parent) else { return false };
            names.push((parent, offset + NAME..offset + NAME + length));
        }
        if next == 0 {
            break;
        }
        if next < NAME + length {
            return false;
        }
        offset += next;
    }
    true
}

fn header<T: Copy>(bytes: &[u8], offset: usize) -> Option<T> {
    let data = bytes.get(offset..offset.checked_add(size_of::<T>())?)?;
    // SAFETY: these FSCTL structs contain only integers and the entire struct is in the buffer.
    Some(unsafe { std::ptr::read_unaligned(data.as_ptr().cast()) })
}

fn invalid_layout() -> windows_core::Error {
    windows_core::Error::new(ERROR_INVALID_DATA.to_hresult(), "incomplete NTFS file layout")
}
