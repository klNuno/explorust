//! Inbound commands from the running shell. The keyboard hook and legacy message window
//! run on separate threads so target windows cannot stall their message loops.

use bus::keys_pipe::{self, BUFFER, RECORD};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::Win32::Foundation::{
    ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_NOREMOVE, PeekMessageW};
use windows::core::{Error, HSTRING, PWSTR};

fn current_user_sid() -> windows::core::Result<HSTRING> {
    // SAFETY: this process's token is owned locally. The aligned buffer remains alive while
    // TOKEN_USER and its SID are read. ConvertSidToStringSidW allocates the resulting string.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = OwnedHandle::from_raw_handle(token.0);
        let token = HANDLE(token.as_raw_handle());
        let mut bytes = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut bytes);
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(size_of::<u64>())];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
        let text = HSTRING::from_wide(sid.as_wide());
        let _ = LocalFree(Some(HLOCAL(sid.0.cast())));
        Ok(text)
    }
}

fn create(shell: u32) -> windows::core::Result<OwnedHandle> {
    let user = current_user_sid()?;
    // UIAccess can have a higher integrity label than the shell. The explicit Medium label
    // permits the shell to write and still enforces no-write-up for callers below Medium.
    let sddl = HSTRING::from(format!("D:P(A;;GA;;;SY)(A;;GA;;;{user})S:(ML;;NW;;;ME)"));
    // SAFETY: CreateNamedPipeW copies the descriptor. Capture its error before freeing it.
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut sd,
            None,
        )?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let handle = CreateNamedPipeW(
            &keys_pipe::name(shell),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            BUFFER as u32,
            0,
            Some(&sa),
        );
        let result = if handle.is_invalid() {
            Err(Error::from_thread())
        } else {
            Ok(OwnedHandle::from_raw_handle(handle.0))
        };
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        result
    }
}

pub fn spawn(shell: u32) -> Result<(), String> {
    let process = if shell == std::process::id() {
        None
    } else {
        // SAFETY: the owned handle keeps the process identity for this helper's lifetime.
        let process = unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                shell,
            )
            .map_err(|e| format!("keys pipe: open shell pid {shell}: {e}"))?;
            OwnedHandle::from_raw_handle(handle.0)
        };
        if !keys_pipe::sibling_image(HANDLE(process.as_raw_handle()), "explorust.exe") {
            return Err(format!(
                "keys pipe: refused shell pid {shell} (expected sibling explorust.exe)"
            ));
        }
        Some(process)
    };
    let pipe =
        create(shell).map_err(|e| format!("keys pipe: create for shell pid {shell}: {e}"))?;
    std::thread::Builder::new()
        .name("keys-pipe".into())
        .spawn(move || serve(pipe, shell, process))
        .map_err(|e| format!("keys pipe thread: {e}"))?;
    crate::hook::log_line(&format!("keys pipe: listening for shell pid {shell}"));
    Ok(())
}

fn serve(pipe: OwnedHandle, shell: u32, process: Option<OwnedHandle>) {
    let pipe = HANDLE(pipe.as_raw_handle());
    let mut message = vec![0u8; BUFFER];
    // SAFETY: create this thread's input queue before Activate uses AttachThreadInput.
    let _ = unsafe { PeekMessageW(&mut MSG::default(), None, 0, 0, PM_NOREMOVE) };
    loop {
        // SAFETY: the sole server thread owns this pipe instance and has no overlapped I/O.
        match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
            Err(e) if e.code() == ERROR_NO_DATA.to_hresult() => {
                let _ = unsafe { DisconnectNamedPipe(pipe) };
                continue;
            }
            Err(e) => {
                crate::hook::log_line(&format!("keys pipe: accept failed: {e}"));
                return;
            }
        }
        let mut client = 0;
        // SAFETY: query the endpoint the kernel connected and check that the pinned shell lives.
        let identified = unsafe { GetNamedPipeClientProcessId(pipe, &mut client) }.is_ok();
        let shell_alive = process.as_ref().is_none_or(|p| {
            (unsafe { WaitForSingleObject(HANDLE(p.as_raw_handle()), 0) }) == WAIT_TIMEOUT
        });
        if identified && client == shell && shell_alive {
            crate::hook::log_line(&format!(
                "keys pipe: accepted explorust client pid {client}"
            ));
            read_commands(pipe, &mut message);
        } else {
            crate::hook::log_line(&format!(
                "keys pipe: refused client pid {client}, expected live shell pid {shell}"
            ));
        }
        let _ = unsafe { DisconnectNamedPipe(pipe) };
    }
}

fn read_commands(pipe: HANDLE, message: &mut [u8]) {
    loop {
        let mut len = 0;
        // SAFETY: the handle stays open and the buffer is exclusively owned by the pipe thread.
        if unsafe { ReadFile(pipe, Some(message), Some(&mut len), None) }.is_err() {
            return;
        }
        let bytes = &message[..len as usize];
        let (records, tail) = bytes.as_chunks::<RECORD>();
        let commands: Option<Vec<_>> = records.iter().map(keys_pipe::decode).collect();
        match commands {
            Some(commands) if !commands.is_empty() && tail.is_empty() => {
                for (command, window) in commands {
                    crate::activate::run(command, window);
                }
            }
            _ => {
                crate::hook::log_line("keys pipe: refused malformed command message");
                return;
            }
        }
    }
}
