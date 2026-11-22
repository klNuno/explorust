//! One-shot Files window handoff between two shells supervised by the same guard process.

use std::{fs, io, path::Path};

const FILE: &str = "reopen.txt";

#[cfg(target_os = "windows")]
pub fn guard_stamp(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return None;
    }
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    // SAFETY: the handle is closed and each output points to its own FILETIME.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let result = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(process);
        result.ok()?;
    }
    Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

pub fn save(dir: &Path, guard: u64, folders: &[String]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let staged = dir.join(format!("reopen-{}.tmp", std::process::id()));
    let mut contents = guard.to_string();
    for folder in folders.iter().filter(|s| !s.is_empty() && !s.contains('\0')) {
        // A parsing name can contain a newline. A NUL cannot appear in a Windows shell name.
        contents.push('\0');
        contents.push_str(folder);
    }
    let result = fs::write(&staged, contents).and_then(|()| fs::rename(&staged, dir.join(FILE)));
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

pub fn take(dir: &Path, guard: u64) -> io::Result<Vec<String>> {
    let path = dir.join(FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut parts = contents.split('\0');
    // A stale handoff from a guard that fell back cannot affect an unrelated sign-in.
    if parts.next().and_then(|stamp| stamp.parse().ok()) != Some(guard) {
        return Ok(Vec::new());
    }
    fs::remove_file(path)?;
    Ok(parts.filter(|s| !s.is_empty()).map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopens_each_named_window_once_under_the_same_guard() {
        let dir = std::env::temp_dir().join(format!("explorust-reopen-test-{}", std::process::id()));
        let folder = r"C:\Windows\Web".to_owned();
        let namespace = "::{some-namespace}\\Échantillon\nsecond line".to_owned();
        save(&dir, 100, &[folder.clone(), namespace.clone(), folder.clone(), String::new()]).unwrap();
        assert!(take(&dir, 200).unwrap().is_empty());
        assert!(dir.join(FILE).is_file());
        assert_eq!(take(&dir, 100).unwrap(), [folder.clone(), namespace, folder]);
        assert!(take(&dir, 100).unwrap().is_empty());
        assert!(!dir.join(FILE).exists());
        fs::remove_dir(dir).unwrap();
    }
}
