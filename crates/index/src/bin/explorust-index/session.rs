//! Keep the SCM service resident, but give the index to an expendable process. Ending the job
//! releases the table, volume handles, blocked journal reads and in-flight clients together.

use crate::{cache, log, pipe, server};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenEventW, OpenProcess, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    STARTUPINFOEXW, SYNCHRONIZATION_ACCESS_RIGHTS, SetEvent, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows_core::{HSTRING, PCWSTR, PWSTR};

const POLL_MS: u32 = 750;
const RELOAD_GRACE: Duration = Duration::from_secs(6);
const RETRY: Duration = Duration::from_secs(5);
const SYNC: SYNCHRONIZATION_ACCESS_RIGHTS = SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000);
const SESSION_CHANGE: PCWSTR = windows_core::w!(r"Local\explorust-index-session-change");
const VOLUME_CHANGE: PCWSTR = windows_core::w!(r"Local\explorust-index-volume-change");

/// Takes ownership of a fresh Win32 handle returned by a successful API call.
unsafe fn owned(h: HANDLE) -> OwnedHandle {
    // SAFETY: callers hand over their sole ownership of the handle.
    unsafe { OwnedHandle::from_raw_handle(h.0) }
}

fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

pub struct Event(OwnedHandle);

impl Event {
    pub fn create(name: PCWSTR) -> windows_core::Result<Self> {
        // SAFETY: default security, auto reset, initially clear.
        unsafe { CreateEventW(None, false, false, name).map(|h| Self(owned(h))) }
    }

    fn open(name: PCWSTR) -> windows_core::Result<Self> {
        // SAFETY: the worker only waits on this event and closes its own handle.
        unsafe { OpenEventW(SYNC, false, name).map(|h| Self(owned(h))) }
    }

    pub fn signal(&self) {
        // SAFETY: the service retains the event for its lifetime.
        let _ = unsafe { SetEvent(raw(&self.0)) };
    }
}

pub struct Notices {
    pub stop: Event,
    pub session: Event,
    pub volume: Event,
}

impl Notices {
    pub fn new() -> windows_core::Result<Self> {
        Ok(Self {
            stop: Event::create(PCWSTR::null())?,
            session: Event::create(SESSION_CHANGE)?,
            volume: Event::create(VOLUME_CHANGE)?,
        })
    }
}

pub fn run_worker() -> u32 {
    let (Ok(session), Ok(volume)) = (Event::open(SESSION_CHANGE), Event::open(VOLUME_CHANGE))
    else {
        return 1;
    };
    cache::enable();
    server::run(move || {
        let _ = std::thread::Builder::new()
            .name("index-session".into())
            .spawn(move || {
                // SAFETY: this thread owns the event handle.
                while unsafe { WaitForSingleObject(raw(&session.0), u32::MAX) } == WAIT_OBJECT_0 {
                    pipe::refresh();
                }
            });
        let _ = std::thread::Builder::new()
            .name("index-volume".into())
            .spawn(move || {
                // SAFETY: this thread owns the event handle.
                while unsafe { WaitForSingleObject(raw(&volume.0), u32::MAX) } == WAIT_OBJECT_0 {
                    // Mount Manager can announce the device before its letter is assigned.
                    for _ in 0..5 {
                        std::thread::sleep(Duration::from_secs(2));
                        if server::rescan() > 0 {
                            break;
                        }
                    }
                }
            });
    });
    0
}

/// Toolhelp enumerates all user sessions, including disconnected sessions with a live shell.
/// Check the image path to distinguish the installed shell from another copy with the same name.
fn shell_running(path: &Path) -> windows_core::Result<bool> {
    // SAFETY: the snapshot and each process handle are local and closed on every exit path.
    unsafe {
        let snapshot = owned(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(raw(&snapshot), &mut entry)?;
        loop {
            let name = entry
                .szExeFile
                .split(|&w| w == 0)
                .next()
                .unwrap_or_default();
            let mut session = 0;
            if name.len() == "explorust.exe".len()
                && String::from_utf16_lossy(name).eq_ignore_ascii_case("explorust.exe")
                && ProcessIdToSessionId(entry.th32ProcessID, &mut session).is_ok()
                && session != 0
                && let Ok(process) = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    entry.th32ProcessID,
                )
            {
                let process = owned(process);
                let mut image = [0u16; 1024];
                let mut len = image.len() as u32;
                if QueryFullProcessImageNameW(
                    raw(&process),
                    PROCESS_NAME_WIN32,
                    PWSTR(image.as_mut_ptr()),
                    &mut len,
                )
                .is_ok()
                    && std::ffi::OsString::from_wide(&image[..len as usize])
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&path.to_string_lossy())
                {
                    return Ok(true);
                }
            }
            if let Err(e) = Process32NextW(raw(&snapshot), &mut entry) {
                return if e.code() == windows::Win32::Foundation::ERROR_NO_MORE_FILES.to_hresult() {
                    Ok(false)
                } else {
                    Err(e)
                };
            }
        }
    }
}

struct Worker {
    job: OwnedHandle,
    process: OwnedHandle,
}

struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the initialized list is deleted before its backing storage goes away.
        unsafe { DeleteProcThreadAttributeList(self.0) };
    }
}

impl Worker {
    fn start(exe: &Path) -> io::Result<Self> {
        let mut cmd: Vec<u16> = std::ffi::OsStr::new("\"")
            .encode_wide()
            .chain(exe.as_os_str().encode_wide())
            .chain(std::ffi::OsStr::new("\" --worker").encode_wide())
            .chain([0])
            .collect();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: the job and attribute storage outlive CreateProcessW, which joins the child
        // to the job before it can run. Returned handles are owned solely by this function.
        unsafe {
            let job = owned(CreateJobObjectW(None, PCWSTR::null())?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                size_of_val(&limits) as u32,
            )?;
            let job_handle = raw(&job);
            let mut bytes = 0;
            let _ = InitializeProcThreadAttributeList(None, 1, None, &mut bytes);
            let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
            InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes)?;
            let attributes = Attributes(list);
            UpdateProcThreadAttribute(
                attributes.0,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some((&job_handle as *const HANDLE).cast()),
                size_of::<HANDLE>(),
                None,
                None,
            )?;
            let mut startup = STARTUPINFOEXW {
                lpAttributeList: attributes.0,
                ..Default::default()
            };
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            CreateProcessW(
                &HSTRING::from(exe.as_os_str()),
                Some(PWSTR(cmd.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut info,
            )?;
            let _thread = owned(info.hThread);
            Ok(Self {
                job,
                process: owned(info.hProcess),
            })
        }
    }

    fn running(&self) -> bool {
        // SAFETY: a zero-time wait on the process handle retained above.
        unsafe { WaitForSingleObject(raw(&self.process), 0) == WAIT_TIMEOUT }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // SAFETY: the job only owns this service's worker; ensure its image is released for setup.
        unsafe {
            if self.running() {
                let _ = TerminateJobObject(raw(&self.job), 0);
                let _ = WaitForSingleObject(raw(&self.process), 3000);
            }
        }
    }
}

pub fn supervise(stop: &Event, stopping: &AtomicBool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let shell = exe.with_file_name("explorust.exe");
    let mut worker: Option<Worker> = None;
    let mut absent_since: Option<Instant> = None;
    let mut retry_at = Instant::now();
    while !stopping.load(Ordering::Acquire) {
        let now = Instant::now();
        if worker.as_ref().is_some_and(|w| !w.running()) {
            worker = None;
            retry_at = now + RETRY;
            log::line("index: worker exited; will restart if the shell is running");
        }
        match shell_running(&shell) {
            Ok(true) => {
                absent_since = None;
                if worker.is_none() && now >= retry_at {
                    match Worker::start(&exe) {
                        Ok(w) => {
                            log::line("index: explorust shell detected, worker started");
                            worker = Some(w);
                        }
                        Err(e) => {
                            log::line(&format!("index: failed to start worker: {e}"));
                            retry_at = now + RETRY;
                        }
                    }
                }
            }
            Ok(false) if worker.is_some() => {
                let absent = absent_since.get_or_insert(now);
                if now.duration_since(*absent) >= RELOAD_GRACE {
                    worker = None;
                    log::line("index: no explorust shell remains, worker stopped");
                }
            }
            _ => {}
        }
        // SAFETY: the stop event is owned by the service and outlives this loop.
        if unsafe { WaitForSingleObject(raw(&stop.0), POLL_MS) } == WAIT_OBJECT_0 {
            break;
        }
    }
    Ok(())
}
