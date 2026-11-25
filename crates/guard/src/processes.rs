//! Point-in-time process snapshots restricted to the caller's Windows session.

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;

#[derive(Clone, Copy)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
}

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn in_session(image: &str) -> Result<Vec<Process>, String> {
    let mut session = 0;
    // SAFETY: the snapshot is owned, and outputs are live for each call.
    unsafe {
        ProcessIdToSessionId(std::process::id(), &mut session)
            .map_err(|e| format!("read the Windows session: {e}"))?;
        let snapshot = Snapshot(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|e| format!("list processes: {e}"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut entry).map_err(|e| format!("read first process: {e}"))?;
        let mut out = Vec::new();
        loop {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(image) {
                let mut process_session = 0;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut process_session).is_ok()
                    && process_session == session
                {
                    out.push(Process {
                        pid: entry.th32ProcessID,
                        parent: entry.th32ParentProcessID,
                    });
                }
            }
            match Process32NextW(snapshot.0, &mut entry) {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(out),
                Err(e) => return Err(format!("read next process: {e}")),
            }
        }
    }
}
