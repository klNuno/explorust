//! In-process bench counters: startup to first frame, private working set, CPU time,
//! GPU memory. Results append to `bench.log` beside the exe.

use std::io::Write;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
};
use windows::Win32::System::ProcessStatus::QueryWorkingSet;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows_core::Interface;

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

/// Milliseconds since the OS created this process.
pub fn ms_since_process_start() -> f64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = Default::default();
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        (ft(GetSystemTimePreciseAsFileTime()) - ft(c)) as f64 / 10_000.0
    }
}

/// Kernel + user CPU time of this process, in milliseconds.
pub fn cpu_ms() -> f64 {
    unsafe {
        let (mut c, mut e, mut k, mut u) = Default::default();
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        (ft(k) + ft(u)) as f64 / 10_000.0
    }
}

/// Private working set in KiB: resident pages not shared with other processes.
pub fn private_ws_kib() -> u64 {
    unsafe {
        let mut buf: Vec<usize> = vec![0; 1 << 16];
        loop {
            let bytes = (buf.len() * size_of::<usize>()) as u32;
            if QueryWorkingSet(GetCurrentProcess(), buf.as_mut_ptr().cast(), bytes).is_ok() {
                break;
            }
            let need = buf[0] + 1024;
            buf.resize(need * 2, 0);
        }
        let n = buf[0];
        let private = buf[1..=n]
            .iter()
            .filter(|&&e| (e >> 8) & 1 == 0) // bit 8 of PSAPI_WORKING_SET_BLOCK is Shared
            .count();
        private as u64 * 4
    }
}

/// This process's local + non-local video memory over every adapter, in KiB.
pub fn gpu_kib() -> u64 {
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return 0 };
        let mut total = 0;
        let mut i = 0;
        while let Ok(a) = f.EnumAdapters1(i) {
            i += 1;
            if let Ok(a3) = a.cast::<IDXGIAdapter3>() {
                for g in [DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL] {
                    let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                    if a3.QueryVideoMemoryInfo(0, g, &mut info).is_ok() {
                        total += info.CurrentUsage / 1024;
                    }
                }
            }
        }
        total
    }
}

/// Appends one line to bench.log beside the exe.
pub fn log(line: &str) {
    let path = std::env::current_exe().map(|p| p.with_file_name("bench.log")).unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Runs the idle bench on a thread: at 10 s logs the working set and GPU memory, then CPU
/// over the next 30 s, then calls `done`. `tag` names the candidate.
pub fn spawn_idle_bench(tag: &'static str, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let ws = private_ws_kib();
        let gpu = gpu_kib();
        let c0 = cpu_ms();
        std::thread::sleep(std::time::Duration::from_secs(30));
        let c1 = cpu_ms();
        log(&format!("{tag} private_ws_kib={ws} gpu_kib={gpu} cpu_ms_30s={:.1} cpu_pct={:.3}", c1 - c0, (c1 - c0) / 300.0));
        done();
    });
}
