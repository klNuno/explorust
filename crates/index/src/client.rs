//! The client side of the pipe, for Start: one connection kept open and reopened once when the
//! service restarted. The service must run in session 0 (a service, not a user process that
//! grabbed the pipe name first), checked on each connect.

use crate::proto::{self, Reply};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;

/// FILE_GENERIC_READ plus FILE_WRITE_DATA: what the pipe's ACL grants the user (no
/// FILE_APPEND_DATA, which on a pipe is the right to create an instance).
pub const ACCESS: u32 = 0x0012_008B;

#[derive(Default)]
pub struct Client {
    pipe: Option<File>,
}

fn connect() -> io::Result<File> {
    let f = OpenOptions::new().read(true).write(true).access_mode(ACCESS).open(crate::PIPE)?;
    let h = HANDLE(f.as_raw_handle());
    let (mut pid, mut session) = (0u32, u32::MAX);
    // SAFETY: queries on a handle owned by `f`, writing into locals.
    unsafe {
        GetNamedPipeServerProcessId(h, &mut pid).map_err(io::Error::other)?;
        ProcessIdToSessionId(pid, &mut session).map_err(io::Error::other)?;
    }
    if session != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("pipe served by pid {pid} in session {session}, not a service")));
    }
    Ok(f)
}

impl Client {
    pub fn connected(&self) -> bool {
        self.pipe.is_some()
    }

    /// The best `max` hits for `q` (case does not matter).
    pub fn query(&mut self, q: &str, max: u16) -> io::Result<Reply> {
        let mut last = None;
        for _ in 0..2 {
            if self.pipe.is_none() {
                self.pipe = Some(connect()?);
            }
            let Some(p) = self.pipe.as_mut() else { break };
            match proto::write_request(p, q, max).and_then(|()| proto::read_reply(p)) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    self.pipe = None;
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("no pipe")))
    }
}
