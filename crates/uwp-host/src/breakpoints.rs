//! explorust dev probe: software breakpoints in modules loaded in this process, armed from
//! `%LOCALAPPDATA%\explorust\bp.txt`, one `<module> <rva hex> <label>` per line (RVAs read from
//! the module's public PDB). Each hit logs its label, thread, first three arguments and the
//! unwound call stack as `module+rva`, at most `MAX_HITS` times per breakpoint, then runs the
//! original instruction (byte restored, one single step, int3 written back). No file, no effect.
//!
//! Used to find who hides the notification center (notify-13): hits land in uwp-host.log. A
//! `winevents` line also logs foreground, focus, cloak and minimize events of every process.
//! Symbolize the hits with the public PDBs (pdb crate, `rva name` per public symbol).

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, FlushInstructionCache,
    RtlLookupFunctionEntry, RtlVirtualUnwind, UNW_FLAG_NHANDLER,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetForegroundWindow};
use windows_core::{HSTRING, PCWSTR};

const MAX_HITS: u32 = 40;
const INT3: u8 = 0xcc;
const BREAKPOINT: u32 = 0x8000_0003;
const SINGLE_STEP: u32 = 0x8000_0004;
const TRAP_FLAG: u32 = 0x100;
const CONTINUE_EXECUTION: i32 = -1;
const CONTINUE_SEARCH: i32 = 0;

struct Breakpoint {
    label: String,
    /// `peek=<offset>:<slot>`: also logs the target of vtable slot `slot` of the interface stored
    /// at `rcx + offset` (which object a method is about to call).
    peek: Option<(usize, usize)>,
    original: u8,
    hits: AtomicU32,
}

static BREAKPOINTS: OnceLock<HashMap<usize, Breakpoint>> = OnceLock::new();

thread_local! {
    /// The breakpoint this thread is single stepping over, to write its int3 back.
    static REARM: Cell<usize> = const { Cell::new(0) };
}

/// Arms the breakpoints of `bp.txt`, once per process; logs what it armed.
pub(crate) fn arm_from_file() {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let Ok(text) = std::fs::read_to_string(base.join("explorust").join("bp.txt")) else { return };
    if BREAKPOINTS.get().is_some() {
        return;
    }
    let mut map = HashMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let mut parts = line.split_whitespace();
        let (Some(module), Some(rva)) = (parts.next(), parts.next()) else { continue };
        let label = parts.next().unwrap_or(rva).to_string();
        let peek = parts.next().and_then(|p| p.strip_prefix("peek=")).and_then(|p| {
            let (o, slot) = p.split_once(':')?;
            Some((usize::from_str_radix(o.trim_start_matches("0x"), 16).ok()?, slot.parse().ok()?))
        });
        let Ok(rva) = usize::from_str_radix(rva.trim_start_matches("0x"), 16) else { continue };
        // SAFETY: looks up an already loaded module by name, no reference taken.
        let Ok(handle) = (unsafe { GetModuleHandleW(&HSTRING::from(module)) }) else {
            hlog!("phase=bp module={module} result=not-loaded");
            continue;
        };
        let addr = handle.0 as usize + rva;
        // SAFETY: `addr` lies in the loaded image (an RVA from its own PDB); reading one byte.
        let original = unsafe { std::ptr::read_volatile(addr as *const u8) };
        map.insert(addr, Breakpoint { label, peek, original, hits: AtomicU32::new(0) });
    }
    if text.lines().any(|l| l.trim() == "winevents") {
        std::thread::Builder::new().name("bp-winevents".into()).spawn(winevents).ok();
    }
    let count = map.len();
    let _ = BREAKPOINTS.set(map);
    // SAFETY: registers a process-wide handler that only acts on our own breakpoint addresses.
    unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
    for addr in BREAKPOINTS.get().into_iter().flat_map(|m| m.keys()) {
        patch(*addr, INT3);
    }
    hlog!("phase=bp armed={count}");
}

fn patch(addr: usize, byte: u8) {
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // SAFETY: `addr` is one of our breakpoint addresses in a loaded image; the page is made
    // writable for the one-byte write and set back, then the instruction cache is flushed.
    unsafe {
        if VirtualProtect(addr as *const c_void, 1, PAGE_EXECUTE_READWRITE, &mut old).is_err() {
            return;
        }
        std::ptr::write_volatile(addr as *mut u8, byte);
        let _ = VirtualProtect(addr as *const c_void, 1, old, &mut old);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(addr as *const c_void), 1);
    }
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes valid exception and context records to a vectored handler.
    let (record, context) = unsafe { (&*(*info).ExceptionRecord, &mut *(*info).ContextRecord) };
    let code = record.ExceptionCode.0 as u32;
    if code == BREAKPOINT {
        let addr = record.ExceptionAddress as usize;
        let Some(bp) = BREAKPOINTS.get().and_then(|m| m.get(&addr)) else { return CONTINUE_SEARCH };
        if bp.hits.fetch_add(1, Ordering::Relaxed) < MAX_HITS {
            let peeked = bp.peek.map(|(offset, slot)| {
                // SAFETY: dev probe on a known method: rcx is `this`, the field holds a COM pointer.
                unsafe {
                    let obj = std::ptr::read((context.Rcx as usize + offset) as *const usize);
                    if obj == 0 {
                        return "null".to_string();
                    }
                    let vtbl = std::ptr::read(obj as *const usize);
                    format!("obj={obj:#x} vtbl={} slot{slot}={}", module_offset(vtbl), module_offset(std::ptr::read((vtbl + 8 * slot) as *const usize)))
                }
            });
            hlog!(
                "phase=bp hit={} peek={peeked:?} tid={} rcx={:#x} rdx={:#x} r8={:#x} rax={:#x} rdi={:#x} r12={:#x} foreground={} stack={}",
                bp.label,
                // SAFETY: plain query of the calling thread's id.
                unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
                context.Rcx,
                context.Rdx,
                context.R8,
                context.Rax,
                context.Rdi,
                context.R12,
                foreground_class(),
                stack(context)
            );
        }
        patch(addr, bp.original);
        context.Rip = addr as u64;
        context.EFlags |= TRAP_FLAG;
        REARM.set(addr);
        return CONTINUE_EXECUTION;
    }
    if code == SINGLE_STEP {
        let addr = REARM.replace(0);
        if addr != 0 {
            patch(addr, INT3);
            context.EFlags &= !TRAP_FLAG;
            return CONTINUE_EXECUTION;
        }
    }
    CONTINUE_SEARCH
}

/// Up to 28 frames from `context`, unwound with the images' own unwind data.
fn stack(context: &CONTEXT) -> String {
    let mut c = *context;
    let mut out = String::new();
    for _ in 0..28 {
        let pc = c.Rip;
        if pc == 0 {
            break;
        }
        out.push(' ');
        out.push_str(&module_offset(pc as usize));
        let mut image = 0u64;
        // SAFETY: RtlLookupFunctionEntry reads the loaded images' exception directories.
        let entry = unsafe { RtlLookupFunctionEntry(pc, &mut image, None) };
        if entry.is_null() {
            // A leaf function: the return address is on top of the stack.
            // SAFETY: the stack pointer of a live frame of this thread.
            c.Rip = unsafe { std::ptr::read(c.Rsp as *const u64) };
            c.Rsp += 8;
        } else {
            let mut data = std::ptr::null_mut();
            let mut frame = 0u64;
            // SAFETY: unwinds a copy of the context with the entry found for its pc.
            unsafe { RtlVirtualUnwind(UNW_FLAG_NHANDLER, image, pc, entry, &mut c, &mut data, &mut frame, None) };
        }
    }
    out
}

fn module_offset(pc: usize) -> String {
    let mut module = HMODULE::default();
    // SAFETY: finds the module holding `pc` without taking a reference.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(pc as *const u16),
            &mut module,
        )
    };
    if found.is_err() {
        return format!("{pc:#x}");
    }
    let mut name = [0u16; 260];
    // SAFETY: writes at most the buffer's length.
    let n = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
    let path = String::from_utf16_lossy(&name[..n]);
    let file = path.rsplit('\\').next().unwrap_or(&path).to_string();
    format!("{file}+{:x}", pc - module.0 as usize)
}

fn foreground_class() -> String {
    let mut name = [0u16; 64];
    // SAFETY: reads the foreground window's class into a local buffer.
    let n = unsafe { GetClassNameW(GetForegroundWindow(), &mut name) } as usize;
    String::from_utf16_lossy(&name[..n])
}

/// Logs foreground, focus and cloak changes of every process (out of context), with the window's
/// class, pid and the thread that raised the event, for the `winevents` line of `bp.txt`.
fn winevents() {
    use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowThreadProcessId, MSG, WINEVENT_OUTOFCONTEXT,
    };
    unsafe extern "system" fn on_event(
        _: HWINEVENTHOOK,
        event: u32,
        hwnd: windows::Win32::Foundation::HWND,
        object: i32,
        _child: i32,
        thread: u32,
        _time: u32,
    ) {
        if hwnd.is_invalid() || (event == 0x8005 && object != -4 && object != 0) {
            return;
        }
        let mut pid = 0u32;
        let mut name = [0u16; 64];
        // SAFETY: plain queries of a window handle the system just reported.
        let n = unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            GetClassNameW(hwnd, &mut name)
        } as usize;
        hlog!(
            "phase=bp event={event:#x} hwnd={:#x} class={} pid={pid} from-thread={thread} object={object}",
            hwnd.0 as usize,
            String::from_utf16_lossy(&name[..n])
        );
    }
    // SAFETY: out-of-context hooks delivered to this thread's message loop below.
    unsafe {
        for (lo, hi) in [(0x3u32, 0x3u32), (0x8005, 0x8005), (0x8017, 0x8018), (0x16, 0x17)] {
            SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}
