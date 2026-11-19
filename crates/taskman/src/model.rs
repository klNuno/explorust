//! The process list as Windows 11's Task Manager shows it: apps (a visible window) with their
//! child processes of the same image, background processes, Windows processes; rates from two
//! samples; formatting. Pure, tested off Windows.

use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub image: String,
    /// Display name: the window title's app (file description) or the image.
    pub name: String,
    pub cpu: f64,
    pub mem: u64,
    pub disk: f64,
    pub gpu: f64,
    pub window: bool,
    pub system: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Apps,
    Background,
    Windows,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub section: Section,
    pub name: String,
    pub pids: Vec<u32>,
    pub cpu: f64,
    pub mem: u64,
    pub disk: f64,
    pub gpu: f64,
    /// Index of the main process in the input slice (its icon and path).
    pub lead: usize,
}

/// Groups processes: each app root takes its descendants of the same image; a background root
/// does the same. Sorted by section, then name.
pub fn group(procs: &[Proc]) -> Vec<Group> {
    let by_pid: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    // Root of each process: climb while the parent has the same image.
    let root = |mut i: usize| {
        let mut seen = 0;
        while let Some(&pi) = by_pid.get(&procs[i].ppid) {
            if pi == i || seen > 64 || !procs[pi].image.eq_ignore_ascii_case(&procs[i].image) {
                break;
            }
            i = pi;
            seen += 1;
        }
        i
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut at: HashMap<usize, usize> = HashMap::new();
    for i in 0..procs.len() {
        let r = root(i);
        let g = *at.entry(r).or_insert_with(|| {
            groups.push(Group {
                section: Section::Background,
                name: procs[r].name.clone(),
                pids: Vec::new(),
                cpu: 0.0,
                mem: 0,
                disk: 0.0,
                gpu: 0.0,
                lead: r,
            });
            groups.len() - 1
        });
        let p = &procs[i];
        let gr = &mut groups[g];
        gr.pids.push(p.pid);
        gr.cpu += p.cpu;
        gr.mem += p.mem;
        gr.disk += p.disk;
        gr.gpu = gr.gpu.max(p.gpu);
        if p.window {
            gr.section = Section::Apps;
        }
    }
    for g in &mut groups {
        if g.section == Section::Background && procs[g.lead].system {
            g.section = Section::Windows;
        }
    }
    groups.sort_by(|a, b| a.section.cmp(&b.section).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    groups
}

/// CPU percent of all cores from a delta of process time over a delta of wall time (100 ns).
pub fn cpu_percent(dcpu: i64, dwall: i64, ncpu: u32) -> f64 {
    if dwall <= 0 || ncpu == 0 {
        return 0.0;
    }
    (dcpu.max(0) as f64 / (dwall as f64 * ncpu as f64) * 100.0).clamp(0.0, 100.0)
}

pub fn mb(bytes: u64) -> String {
    let m = bytes as f64 / 1048576.0;
    if m >= 1024.0 { format!("{:.1} GB", m / 1024.0) } else { format!("{m:.1} MB") }
}

pub fn rate(bytes_s: f64) -> String {
    format!("{:.1} MB/s", bytes_s / 1048576.0)
}

pub fn pct(p: f64) -> String {
    format!("{p:.1}%")
}

pub fn mbps(bytes_s: f64) -> String {
    let b = bytes_s * 8.0;
    if b >= 1e6 { format!("{:.1} Mbps", b / 1e6) } else { format!("{:.0} Kbps", b / 1e3) }
}

/// Fixed-length history for the performance graphs (60 samples, the last minute).
#[derive(Clone, Debug, Default)]
pub struct History(pub std::collections::VecDeque<f64>);

impl History {
    pub const LEN: usize = 60;
    pub fn push(&mut self, v: f64) {
        if self.0.len() == Self::LEN {
            self.0.pop_front();
        }
        self.0.push_back(v);
    }
    pub fn last(&self) -> f64 {
        self.0.back().copied().unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, image: &str, window: bool) -> Proc {
        Proc { pid, ppid, image: image.into(), name: image.into(), cpu: 1.0, mem: 10, window, ..Default::default() }
    }

    #[test]
    fn children_of_the_same_image_join_their_app() {
        let procs = vec![p(1, 0, "x.exe", false), p(10, 1, "chrome.exe", true), p(11, 10, "chrome.exe", false), p(12, 11, "chrome.exe", false), p(13, 10, "helper.exe", false)];
        let g = group(&procs);
        let chrome = g.iter().find(|g| g.name == "chrome.exe").unwrap();
        assert_eq!(chrome.section, Section::Apps);
        assert_eq!(chrome.pids, vec![10, 11, 12]);
        assert_eq!(chrome.mem, 30);
        assert_eq!(g[0].name, "chrome.exe");
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn rates_and_formats() {
        assert_eq!(cpu_percent(10_000_000, 10_000_000, 4), 25.0);
        assert_eq!(cpu_percent(5, 0, 4), 0.0);
        assert_eq!(mb(1048576 * 3 / 2), "1.5 MB");
        assert_eq!(mb(1048576 * 2048), "2.0 GB");
        assert_eq!(mbps(125_000.0), "1.0 Mbps");
        let mut h = History::default();
        for i in 0..70 {
            h.push(i as f64);
        }
        assert_eq!(h.0.len(), 60);
        assert_eq!(h.last(), 69.0);
    }
}
