//! Explorer's "group by date modified" buckets. The labels are not exposed by a shell API
//! we can call without a view, so they are hardcoded for French and English.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;

const DAY: u64 = 864_000_000_000;

fn split(t: u64) -> FILETIME {
    FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 }
}

fn local(t: u64) -> (u64, SYSTEMTIME) {
    unsafe {
        let mut l = FILETIME::default();
        let _ = FileTimeToLocalFileTime(&split(t), &mut l);
        let mut st = SYSTEMTIME::default();
        let _ = FileTimeToSystemTime(&l, &mut st);
        (((l.dwHighDateTime as u64) << 32 | l.dwLowDateTime as u64) / DAY, st)
    }
}

/// Bucket index, newest first: 0 today .. 7 a long time ago.
pub fn bucket(modified: u64, now: u64) -> usize {
    let (d, st) = local(modified);
    let (n, sn) = local(now);
    // 1601-01-01 was a Monday, so day % 7 == 0 is a Monday.
    let week_start = n - n % 7;
    let month = |s: &SYSTEMTIME| s.wYear as i32 * 12 + s.wMonth as i32;
    if d >= n {
        0
    } else if d + 1 == n {
        1
    } else if d >= week_start {
        2
    } else if d + 7 >= week_start {
        3
    } else if month(&st) == month(&sn) {
        4
    } else if month(&st) + 1 == month(&sn) {
        5
    } else if st.wYear == sn.wYear {
        6
    } else {
        7
    }
}

pub fn now() -> u64 {
    let f = unsafe { GetSystemTimeAsFileTime() };
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

pub fn label(bucket: usize) -> &'static str {
    const FR: [&str; 8] = [
        "Aujourd\u{2019}hui", "Hier", "Plus t\u{f4}t cette semaine", "Semaine derni\u{e8}re",
        "Plus t\u{f4}t dans le mois", "Le mois dernier", "Plus t\u{f4}t cette ann\u{e9}e", "Il y a longtemps",
    ];
    const EN: [&str; 8] = [
        "Today", "Yesterday", "Earlier this week", "Last week", "Earlier this month", "Last month",
        "Earlier this year", "A long time ago",
    ];
    let fr = unsafe { GetUserDefaultUILanguage() } & 0x3ff == 0x0c;
    if fr { FR[bucket] } else { EN[bucket] }
}

pub fn is_french() -> bool {
    unsafe { GetUserDefaultUILanguage() & 0x3ff == 0x0c }
}
