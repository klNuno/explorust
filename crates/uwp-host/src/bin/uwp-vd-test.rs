//! uwp-vd-test: switches Windows' virtual desktop through the immersive shell's private service,
//! the way the switcher does, and checks that the shell process (explorust.exe, which hosts the
//! immersive shell controller) is still alive afterwards. Run beside a running explorust:
//!
//!   uwp-vd-test.exe [hold_s] [after_s]
//!
//! It creates a second desktop when there is only one, switches to the next desktop
//! (`SwitchDesktop`, slot 9), waits `hold_s` (default 5), switches back, waits again, switches to
//! the next desktop once more and removes it while it is current (Windows falls back to the
//! first one), then watches the shell's pid for `after_s` (default 60). Lines go to stdout and to
//! `%LOCALAPPDATA%\explorust\uwp-vd-test.log`. Exit code 0 when the shell survived.
//!
//! Slots of `IVirtualDesktopManagerInternal` (twinui.pcshell.dll 26300.9974, vftable
//! `CVirtualDesktopManager` for that interface, read from the public PDB): GetCount 3,
//! GetCurrentDesktop 6, GetDesktops 7, SwitchDesktop 9, CreateDesktopW 11, RemoveDesktop 13.
//! `IVirtualDesktop` GetID 4.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() {
    std::process::exit(win::run());
}

#[cfg(target_os = "windows")]
mod win {
    use std::ffi::c_void;
    use std::io::Write;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Com::{
        CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, IServiceProvider,
    };
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    use windows::Win32::UI::Shell::Common::IObjectArray;
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
    use windows::core::{GUID, HRESULT, IUnknown, Interface};

    const CLSID_IMMERSIVE_SHELL: GUID = GUID::from_u128(0xc2f03a33_21f5_47fa_b4bb_156362a2f239);
    const SID_VDM: GUID = GUID::from_u128(0xc5e0cdca_7b6e_41b2_9fc4_d93975cc467b);
    const IID_VDMI: GUID = GUID::from_u128(0x53f5ca0b_158f_4124_900c_057158060b27);
    const IID_VD: GUID = GUID::from_u128(0x3f07f4be_b107_441a_af0f_39d82529072c);

    fn say(msg: &str) {
        // SAFETY: GetLocalTime only writes the returned struct.
        let t = unsafe { GetLocalTime() };
        let text = format!(
            "{:02}:{:02}:{:02}.{:03} [{}] {msg}",
            t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, std::process::id()
        );
        println!("{text}");
        let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join("explorust");
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("uwp-vd-test.log")) {
            let _ = writeln!(f, "{text}\r");
        }
    }

    /// SAFETY: `obj` is a live COM object whose vtable has slot `i` of type `F`.
    unsafe fn slot<F: Copy>(obj: &IUnknown, i: usize) -> F {
        unsafe {
            let vtbl = *(obj.as_raw() as *const *const usize);
            std::mem::transmute_copy(&*vtbl.add(i))
        }
    }

    fn owned(hr: HRESULT, raw: *mut c_void) -> windows::core::Result<IUnknown> {
        hr.ok()?;
        if raw.is_null() {
            return Err(HRESULT(0x80004003u32 as i32).into());
        }
        // SAFETY: an owned reference returned by a successful call.
        Ok(unsafe { IUnknown::from_raw(raw) })
    }

    type Out = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
    type One = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
    type Two = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;

    fn id(desk: &IUnknown) -> GUID {
        let mut id = GUID::zeroed();
        // SAFETY: IVirtualDesktop slot 4, GetID(GUID*).
        unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT = slot(desk, 4);
            let _ = f(desk.as_raw(), &mut id);
        }
        id
    }

    fn desktops(mgr: &IUnknown) -> windows::core::Result<Vec<IUnknown>> {
        // SAFETY: slot 7, GetDesktops(IObjectArray**); items answer IVirtualDesktop.
        unsafe {
            let f: Out = slot(mgr, 7);
            let mut raw = std::ptr::null_mut();
            let arr: IObjectArray = owned(f(mgr.as_raw(), &mut raw), raw)?.cast()?;
            let mut out = Vec::new();
            for i in 0..arr.GetCount()? {
                let d: IUnknown = arr.GetAt(i)?;
                let mut raw = std::ptr::null_mut();
                out.push(owned(d.query(&IID_VD, &mut raw), raw)?);
            }
            Ok(out)
        }
    }

    fn current(mgr: &IUnknown) -> windows::core::Result<GUID> {
        // SAFETY: slot 6, GetCurrentDesktop(IVirtualDesktop**).
        unsafe {
            let f: Out = slot(mgr, 6);
            let mut raw = std::ptr::null_mut();
            Ok(id(&owned(f(mgr.as_raw(), &mut raw), raw)?))
        }
    }

    fn switch(mgr: &IUnknown, to: &IUnknown) -> windows::core::Result<()> {
        // SAFETY: slot 9, SwitchDesktop(IVirtualDesktop*).
        unsafe {
            let f: One = slot(mgr, 9);
            f(mgr.as_raw(), to.as_raw()).ok()
        }
    }

    /// Waits up to `d` for the process to end; true when it is still alive after `d`.
    fn alive_after(pid: u32, d: Duration) -> bool {
        // SAFETY: a SYNCHRONIZE handle of the shell process, closed below.
        let Ok(h) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
            return false;
        };
        let alive = unsafe { WaitForSingleObject(h, d.as_millis() as u32) } == WAIT_TIMEOUT;
        let _ = unsafe { CloseHandle(h) };
        alive
    }

    pub fn run() -> i32 {
        let mut args = std::env::args().skip(1).filter_map(|a| a.parse::<u64>().ok());
        let hold = Duration::from_secs(args.next().unwrap_or(5));
        let after = Duration::from_secs(args.next().unwrap_or(60));
        let mut shell_pid = 0u32;
        // SAFETY: plain queries of the session's shell window.
        unsafe { GetWindowThreadProcessId(GetShellWindow(), Some(&mut shell_pid)) };
        say(&format!("shell pid {shell_pid}"));
        if shell_pid == 0 {
            return 2;
        }
        // SAFETY: this thread's COM apartment for the whole run.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let result = (|| -> windows::core::Result<()> {
            // SAFETY: COM creation and QueryService with checked out pointers.
            let mgr = unsafe {
                let shell: IServiceProvider = CoCreateInstance(&CLSID_IMMERSIVE_SHELL, None, CLSCTX_LOCAL_SERVER)?;
                let mut raw = std::ptr::null_mut();
                owned((shell.vtable().QueryService)(shell.as_raw(), &SID_VDM, &IID_VDMI, &mut raw), raw)?
            };
            let mut desks = desktops(&mgr)?;
            let start = current(&mgr)?;
            say(&format!("{} desktops, current {start:?}", desks.len()));
            let mut created = None;
            if desks.len() < 2 {
                // SAFETY: slot 11, CreateDesktopW(IVirtualDesktop**).
                let d = unsafe {
                    let f: Out = slot(&mgr, 11);
                    let mut raw = std::ptr::null_mut();
                    owned(f(mgr.as_raw(), &mut raw), raw)?
                };
                say(&format!("created desktop {:?}", id(&d)));
                created = Some(d);
                desks = desktops(&mgr)?;
            }
            let at = desks.iter().position(|d| id(d) == start).unwrap_or(0);
            let next = &desks[(at + 1) % desks.len()];
            let back = &desks[at];
            let t = Instant::now();
            switch(&mgr, next)?;
            say(&format!(
                "SwitchDesktop to {:?} in {:.1} ms, current now {:?}",
                id(next),
                t.elapsed().as_secs_f64() * 1000.0,
                current(&mgr)?
            ));
            if !alive_after(shell_pid, hold) {
                say("shell died while switched");
                return Ok(());
            }
            let t = Instant::now();
            switch(&mgr, back)?;
            say(&format!(
                "SwitchDesktop back in {:.1} ms, current now {:?}",
                t.elapsed().as_secs_f64() * 1000.0,
                current(&mgr)?
            ));
            if !alive_after(shell_pid, hold) {
                say("shell died after the switch back");
                return Ok(());
            }
            // Removal of the current desktop: Windows switches to the fallback itself.
            switch(&mgr, next)?;
            say(&format!("SwitchDesktop to {:?} again, current now {:?}", id(next), current(&mgr)?));
            let t = Instant::now();
            // SAFETY: slot 13, RemoveDesktop(IVirtualDesktop*, IVirtualDesktop* fallback).
            let hr = unsafe {
                let f: Two = slot(&mgr, 13);
                f(mgr.as_raw(), next.as_raw(), back.as_raw())
            };
            say(&format!(
                "RemoveDesktop of the current desktop: {hr:?} in {:.1} ms, current now {:?}, {} desktops",
                t.elapsed().as_secs_f64() * 1000.0,
                current(&mgr)?,
                desktops(&mgr)?.len()
            ));
            drop(created);
            Ok(())
        })();
        if let Err(e) = result {
            say(&format!("service error: {e}"));
        }
        let survived = alive_after(shell_pid, after);
        say(&format!("shell pid {shell_pid} alive {} s later: {survived}", after.as_secs()));
        if survived { 0 } else { 1 }
    }
}
