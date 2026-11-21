use super::restore_records;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Mutex;
use windows::Win32::Foundation::{GENERIC_WRITE, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_MODE, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    WriteFile,
};
use windows::Win32::System::Pipes::{
    GetNamedPipeServerProcessId, PIPE_NOWAIT, SetNamedPipeHandleState,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};

pub fn name(shell: u32) -> HSTRING {
    HSTRING::from(format!(r"\\.\pipe\explorust-keys-{shell}"))
}

/// The peer must be the expected executable beside this one. Use Windows' ordinal path
/// comparison, keeping the full UTF-16 path from the process handle.
pub fn sibling_image(process: HANDLE, filename: &str) -> bool {
    let Ok(expected) = std::env::current_exe().map(|path| path.with_file_name(filename)) else {
        return false;
    };
    let expected: Vec<u16> = expected.as_os_str().encode_wide().collect();
    let mut image = vec![0u16; 32768];
    let mut len = image.len() as u32;
    // SAFETY: the query writes into the supplied buffer. WaitForSingleObject checks liveness
    // without blocking and requires the caller to have opened the handle for synchronization.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
            && CompareStringOrdinal(&image[..len as usize], &expected, true) == CSTR_EQUAL
            && WaitForSingleObject(process, 0) == WAIT_TIMEOUT
    }
}

static PIPE: Mutex<Option<OwnedHandle>> = Mutex::new(None);

fn connect(shell: u32) -> Option<OwnedHandle> {
    // SAFETY: the pipe and any process handle are owned here. Identification-level SQOS prevents
    // impersonation as the shell even if the endpoint turns out to belong to another process.
    unsafe {
        let pipe = OwnedHandle::from_raw_handle(
            CreateFileW(
                &name(shell),
                GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
            .ok()?
            .0,
        );
        let handle = HANDLE(pipe.as_raw_handle());
        let mut pid = 0;
        GetNamedPipeServerProcessId(handle, &mut pid).ok()?;
        if pid != shell {
            let peer = OwnedHandle::from_raw_handle(
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
                .ok()?
                .0,
            );
            if !sibling_image(HANDLE(peer.as_raw_handle()), "explorust-keys.exe") {
                return None;
            }
        }
        // On a message pipe a nonblocking write queues the entire message or zero bytes.
        SetNamedPipeHandleState(handle, Some(&PIPE_NOWAIT), None, None).ok()?;
        Some(pipe)
    }
}

pub(crate) fn post(records: &[u8]) -> bool {
    let mut pipe = PIPE.lock().unwrap_or_else(|poison| poison.into_inner());
    let shell = std::process::id();
    for _ in 0..2 {
        if pipe.is_none() {
            *pipe = connect(shell);
        }
        let Some(connected) = pipe.as_ref() else {
            continue;
        };
        let mut written = 0;
        // SAFETY: the handle and the bytes stay valid for this synchronous, nonblocking call.
        let result = unsafe {
            WriteFile(
                HANDLE(connected.as_raw_handle()),
                Some(records),
                Some(&mut written),
                None,
            )
        };
        if result.is_ok() {
            return written as usize == records.len();
        }
        *pipe = None;
    }
    false
}

/// Enqueue all raises and the final foreground request as one pipe message. A failed write leaves
/// no partial batch for the helper to apply.
pub fn restore_batch(windows: &[isize]) -> bool {
    restore_records(windows).is_some_and(|records| post(&records))
}
