//! One sample a second: every process from one NtQuerySystemInformation call, app windows from
//! EnumWindows, GPU engines from PDH, system totals (GetSystemTimes, memory, network octets).

use super::log;
use crate::model::{History, Proc, cpu_percent};
use crate::parse::{gpu_instance, processes};
use std::collections::HashMap;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Performance::*;
use windows::Win32::System::SystemInformation::{GetSystemInfo, GetSystemTimePreciseAsFileTime, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, HSTRING, PWSTR, w};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

#[derive(Clone, Default)]
pub struct Meta {
    pub path: String,
    pub desc: String,
}

pub struct Totals {
    pub cpu: History,
    pub mem: History,
    pub disk: History,
    pub net: History,
    pub gpu: History,
    pub mem_used: u64,
    pub mem_total: u64,
    pub net_in: f64,
    pub net_out: f64,
    pub procs: usize,
    pub threads: u64,
}

pub struct Sampler {
    buf: Vec<u64>,
    prev: HashMap<(u32, i64), (i64, u64)>,
    prev_t: i64,
    ncpu: u32,
    meta: HashMap<(u32, i64), Meta>,
    desc: HashMap<String, String>,
    gpu: Option<(PDH_HQUERY, PDH_HCOUNTER)>,
    sys_prev: (u64, u64, u64),
    net_prev: (u64, u64),
    pub procs: Vec<Proc>,
    pub metas: Vec<Meta>,
    pub totals: Totals,
    pub cost_ms: f64,
}

fn ft(f: FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

fn now() -> i64 {
    unsafe { ft(GetSystemTimePreciseAsFileTime()) as i64 }
}

fn image_path(pid: u32) -> String {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(h);
        if ok { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() }
    }
}

/// FileDescription of an exe from its version resource, as Task Manager names processes.
fn description(path: &str) -> String {
    unsafe {
        let p = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&p, None);
        if size == 0 {
            return String::new();
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(&p, None, size, data.as_mut_ptr().cast()).is_err() {
            return String::new();
        }
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        let mut keys = vec![r"\StringFileInfo\040904B0\FileDescription".to_string()];
        if VerQueryValueW(data.as_ptr().cast(), w!(r"\VarFileInfo\Translation"), &mut ptr, &mut len).as_bool() && len >= 4 {
            let t = ptr as *const u16;
            keys.insert(0, format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", *t, *t.add(1)));
        }
        for k in keys {
            if VerQueryValueW(data.as_ptr().cast(), &HSTRING::from(k), &mut ptr, &mut len).as_bool() && len > 1 {
                let s = std::slice::from_raw_parts(ptr as *const u16, len as usize - 1);
                let s = String::from_utf16_lossy(s).trim().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
        String::new()
    }
}

unsafe extern "system" fn each_window(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        let set = &mut *(lp.0 as *mut Vec<u32>);
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return true.into();
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
        if cloaked != 0 {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        set.push(pid);
        true.into()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let mut si = SYSTEM_INFO::default();
        unsafe { GetSystemInfo(&mut si) };
        let gpu = unsafe {
            let mut q = PDH_HQUERY::default();
            let mut c = PDH_HCOUNTER::default();
            (PdhOpenQueryW(None, 0, &mut q) == 0
                && PdhAddEnglishCounterW(q, w!(r"\GPU Engine(*)\Utilization Percentage"), 0, &mut c) == 0)
                .then(|| {
                    PdhCollectQueryData(q);
                    (q, c)
                })
        };
        log::line(&format!("sampler: {} cpus, gpu counters {}", si.dwNumberOfProcessors, gpu.is_some()));
        let mut s = Sampler {
            buf: vec![0; 1 << 16],
            prev: HashMap::new(),
            prev_t: 0,
            ncpu: si.dwNumberOfProcessors.max(1),
            meta: HashMap::new(),
            desc: HashMap::new(),
            gpu,
            sys_prev: (0, 0, 0),
            net_prev: (0, 0),
            procs: Vec::new(),
            metas: Vec::new(),
            totals: Totals {
                cpu: History::default(),
                mem: History::default(),
                disk: History::default(),
                net: History::default(),
                gpu: History::default(),
                mem_used: 0,
                mem_total: 0,
                net_in: 0.0,
                net_out: 0.0,
                procs: 0,
                threads: 0,
            },
            cost_ms: 0.0,
        };
        s.tick();
        s
    }

    fn query(&mut self) -> Vec<crate::parse::RawProc> {
        loop {
            let len = (self.buf.len() * 8) as u32;
            let mut ret = 0u32;
            let st = unsafe { NtQuerySystemInformation(5, self.buf.as_mut_ptr().cast(), len, &mut ret) };
            if st == 0xC000_0004u32 as i32 {
                self.buf = vec![0; (ret as usize / 8 + 4096).max(self.buf.len() * 2)];
                continue;
            }
            if st != 0 {
                return Vec::new();
            }
            let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, self.buf.len() * 8) };
            return processes(bytes, self.buf.as_ptr() as usize);
        }
    }

    fn gpu_by_pid(&self) -> (HashMap<u32, f64>, f64) {
        let mut per: HashMap<(u32, String), f64> = HashMap::new();
        let Some((q, c)) = self.gpu else { return (HashMap::new(), 0.0) };
        unsafe {
            if PdhCollectQueryData(q) != 0 {
                return (HashMap::new(), 0.0);
            }
            let (mut size, mut count) = (0u32, 0u32);
            PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if size == 0 {
                return (HashMap::new(), 0.0);
            }
            let mut buf = vec![0u64; size as usize / 8 + 1];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(c, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return (HashMap::new(), 0.0);
            }
            for i in 0..count as usize {
                let it = &*items.add(i);
                let name = it.szName.to_string().unwrap_or_default();
                if let Some((pid, eng)) = gpu_instance(&name) {
                    *per.entry((pid, eng.to_string())).or_default() += it.FmtValue.Anonymous.doubleValue;
                }
            }
        }
        let mut by_pid: HashMap<u32, f64> = HashMap::new();
        let mut by_eng: HashMap<String, f64> = HashMap::new();
        for ((pid, eng), v) in per {
            let e = by_pid.entry(pid).or_default();
            *e = e.max(v);
            *by_eng.entry(eng).or_default() += v;
        }
        let total = by_eng.values().fold(0.0f64, |a, &b| a.max(b)).min(100.0);
        (by_pid, total)
    }

    pub fn tick(&mut self) {
        let t0 = std::time::Instant::now();
        let raw = self.query();
        let t = now();
        let dt = t - self.prev_t;
        let mut windows: Vec<u32> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(each_window), LPARAM(&mut windows as *mut _ as isize));
        }
        let (gpu, gpu_total) = self.gpu_by_pid();
        let mut prev = HashMap::with_capacity(raw.len());
        let mut procs = Vec::with_capacity(raw.len());
        let mut metas = Vec::with_capacity(raw.len());
        let mut disk_total = 0.0;
        let mut threads = 0u64;
        for r in raw.iter().filter(|r| r.pid != 0) {
            let key = (r.pid, r.create);
            let (cpu, disk) = match self.prev.get(&key) {
                Some(&(c, io)) if self.prev_t > 0 => (cpu_percent(r.cpu - c, dt, self.ncpu), (r.io.saturating_sub(io)) as f64 / (dt as f64 / 1e7)),
                _ => (0.0, 0.0),
            };
            prev.insert(key, (r.cpu, r.io));
            disk_total += disk;
            threads += r.threads as u64;
            let meta = self.meta.entry(key).or_insert_with(|| {
                let path = image_path(r.pid);
                let desc = if path.is_empty() { String::new() } else { self.desc.entry(path.clone()).or_insert_with(|| description(&path)).clone() };
                Meta { path, desc }
            });
            let lower = meta.path.to_ascii_lowercase();
            // Windows processes: the kernel, protected ones (no path) and images under C:\Windows.
            let system = r.pid == 4 || lower.is_empty() || lower.starts_with(r"c:\windows\");
            procs.push(Proc {
                pid: r.pid,
                ppid: r.ppid,
                image: r.name.clone(),
                name: if meta.desc.is_empty() { r.name.clone() } else { meta.desc.clone() },
                cpu,
                mem: r.private_ws,
                disk,
                gpu: gpu.get(&r.pid).copied().unwrap_or(0.0),
                window: windows.contains(&r.pid),
                system,
            });
            metas.push(meta.clone());
        }
        self.meta.retain(|k, _| prev.contains_key(k));
        self.prev = prev;
        self.prev_t = t;
        self.procs = procs;
        self.metas = metas;
        let tt = &mut self.totals;
        tt.procs = self.procs.len();
        tt.threads = threads;
        unsafe {
            let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
            if windows::Win32::System::Threading::GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
                let (i, k, u) = (ft(idle), ft(kernel), ft(user));
                let (pi, pk, pu) = self.sys_prev;
                if pk > 0 {
                    let busy = (k - pk + u - pu).saturating_sub(i - pi) as f64;
                    let all = (k - pk + u - pu) as f64;
                    tt.cpu.push(if all > 0.0 { busy / all * 100.0 } else { 0.0 });
                }
                self.sys_prev = (i, k, u);
            }
            let mut ms = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            if GlobalMemoryStatusEx(&mut ms).is_ok() {
                tt.mem_total = ms.ullTotalPhys;
                tt.mem_used = ms.ullTotalPhys - ms.ullAvailPhys;
                tt.mem.push(tt.mem_used as f64 / tt.mem_total.max(1) as f64 * 100.0);
            }
            let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
            if GetIfTable2(&mut table).is_ok() && !table.is_null() {
                let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
                let (mut inb, mut outb) = (0u64, 0u64);
                for r in rows.iter().filter(|r| r.Type != 24 && r.InterfaceAndOperStatusFlags._bitfield & 1 != 0) {
                    inb += r.InOctets;
                    outb += r.OutOctets;
                }
                FreeMibTable(table as _);
                let secs = dt as f64 / 1e7;
                if self.net_prev.0 > 0 && secs > 0.0 {
                    tt.net_in = inb.saturating_sub(self.net_prev.0) as f64 / secs;
                    tt.net_out = outb.saturating_sub(self.net_prev.1) as f64 / secs;
                    tt.net.push(tt.net_in + tt.net_out);
                }
                self.net_prev = (inb, outb);
            }
        }
        tt.disk.push(disk_total);
        tt.gpu.push(gpu_total);
        self.cost_ms = t0.elapsed().as_secs_f64() * 1000.0;
    }
}

impl Sampler {
    pub fn ncpu_pub(&self) -> u32 {
        self.ncpu
    }
}
