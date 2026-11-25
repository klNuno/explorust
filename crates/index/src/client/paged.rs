//! Cancellable pipe I/O for Files. Each search owns its connection, so dropping it also tells
//! the service to stop its scan. Start's synchronous persistent connection stays independent.

use crate::proto::{self, Page};
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{ERROR_IO_PENDING, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, ReadFile, WriteFile};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_core::PCWSTR;

/// `Ok(false)` means the service cannot answer this scope. The caller may stop after any page.
pub fn query_pages(
    q: &str,
    folder: &str,
    active: impl Fn() -> bool,
    mut consume: impl FnMut(Page) -> bool,
) -> io::Result<bool> {
    if !active() {
        return Err(cancelled());
    }
    let file = super::connect(FILE_FLAG_OVERLAPPED.0)?;
    // SAFETY: the event is unnamed and ownership of the returned handle is transferred.
    let event = unsafe {
        OwnedHandle::from_raw_handle(
            CreateEventW(None, true, false, PCWSTR::null())
                .map_err(io::Error::other)?
                .0,
        )
    };
    let mut pipe = Pipe {
        file,
        event,
        active,
    };
    proto::write_paged_request(&mut pipe, q, folder)?;
    let mut input = BufReader::with_capacity(64 << 10, pipe);
    while let Some(page) = proto::read_page(&mut input)? {
        let more = page.more;
        if !consume(page) || !more {
            return Ok(true);
        }
    }
    Ok(false)
}

struct Pipe<F> {
    file: File,
    event: OwnedHandle,
    active: F,
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "search cancelled")
}

impl<F: Fn() -> bool> Pipe<F> {
    fn io(
        &self,
        start: impl FnOnce(HANDLE, &mut OVERLAPPED) -> windows_core::Result<()>,
    ) -> io::Result<usize> {
        if !(self.active)() {
            return Err(cancelled());
        }
        let handle = HANDLE(self.file.as_raw_handle());
        let event = HANDLE(self.event.as_raw_handle());
        let mut op = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        let mut count = 0;
        // SAFETY: the handle, OVERLAPPED and I/O buffer stay live until completion or until the
        // cancellation completes. This event serves exactly one operation at a time.
        unsafe {
            ResetEvent(event).map_err(io::Error::other)?;
            match start(handle, &mut op) {
                Ok(()) => {
                    GetOverlappedResult(handle, &op, &mut count, false)
                        .map_err(io::Error::other)?;
                    return Ok(count as usize);
                }
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(e) => return Err(io::Error::other(e)),
            }
            let result = loop {
                if !(self.active)() {
                    break cancelled();
                }
                match WaitForSingleObject(event, 10) {
                    WAIT_OBJECT_0 => {
                        GetOverlappedResult(handle, &op, &mut count, false)
                            .map_err(io::Error::other)?;
                        return Ok(count as usize);
                    }
                    WAIT_TIMEOUT => {}
                    _ => break io::Error::last_os_error(),
                }
            };
            let _ = CancelIoEx(handle, Some(&op));
            let _ = GetOverlappedResult(handle, &op, &mut count, true);
            Err(result)
        }
    }
}

impl<F: Fn() -> bool> Read for Pipe<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `io` waits for the operation before releasing the borrow of the buffer.
        self.io(|h, op| unsafe { ReadFile(h, Some(buf), None, Some(op)) })
    }
}

impl<F: Fn() -> bool> Write for Pipe<F> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `io` waits for the operation before releasing the borrow of the buffer.
        self.io(|h, op| unsafe { WriteFile(h, Some(buf), None, Some(op)) })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PeekNamedPipe,
    };
    use windows_core::HSTRING;

    #[test]
    fn leaving_cancels_pending_pipe_read_and_disconnects() {
        let name = format!(
            r"\\.\pipe\explorust-index-cancellation-{}",
            std::process::id()
        );
        let server = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(&name),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                None,
            )
        };
        assert!(!server.is_invalid());
        let _server = unsafe { OwnedHandle::from_raw_handle(server.0) };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let (started, pending) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OVERLAPPED.0)
                .open(name)
                .unwrap();
            let event = unsafe {
                OwnedHandle::from_raw_handle(
                    CreateEventW(None, true, false, PCWSTR::null()).unwrap().0,
                )
            };
            let checks = std::cell::Cell::new(0);
            let active = || {
                checks.set(checks.get() + 1);
                if checks.get() == 2 {
                    started.send(()).unwrap();
                }
                !cancel.load(Ordering::Acquire)
            };
            let mut pipe = Pipe {
                file,
                event,
                active,
            };
            let error = pipe.read(&mut [0]).unwrap_err();
            drop(pipe);
            finished.send(error.kind()).unwrap();
        });
        if let Err(error) = unsafe { ConnectNamedPipe(server, None) } {
            assert_eq!(error.code(), ERROR_PIPE_CONNECTED.to_hresult());
        }
        pending
            .recv_timeout(Duration::from_secs(5))
            .expect("overlapped read is pending");
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            io::ErrorKind::ConnectionAborted
        );
        assert!(unsafe { PeekNamedPipe(server, None, 0, None, None, None) }.is_err());
        reader.join().unwrap();
    }
}
