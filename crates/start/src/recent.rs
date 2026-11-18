//! Recommended's files: Windows' recent items (`FOLDERID_Recent`), newest first, folders and
//! missing targets left out as Windows does.

use crate::apps::{image, take_pwstr};
use shelldata::Image;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{
    FOLDERID_Recent, IShellItemImageFactory, IShellLinkW, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, ShellLink,
};
use windows_core::{HSTRING, Interface, PCWSTR};

pub struct Recent {
    pub name: String,
    /// The shortcut in the Recent folder, opened with its default verb.
    pub link: String,
    pub icon: Option<Image>,
    /// Unix seconds of the last use.
    pub when: i64,
}

pub fn load(icon_px: i32, max: usize) -> Vec<Recent> {
    // SAFETY: known folder lookup, the string freed by take_pwstr.
    let dir = match unsafe { SHGetKnownFolderPath(&FOLDERID_Recent, KF_FLAG_DEFAULT, None) } {
        Ok(p) => PathBuf::from(take_pwstr(p)),
        Err(_) => return Vec::new(),
    };
    let mut links: Vec<(i64, PathBuf)> = std::fs::read_dir(&dir)
        .map(|it| {
            it.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk")))
                .filter_map(|e| {
                    let t = e.metadata().ok()?.modified().ok()?;
                    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
                    Some((secs, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    links.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = Vec::new();
    for (when, lnk) in links {
        if out.len() >= max {
            break;
        }
        let Some(target) = target(&lnk) else { continue };
        if !target.is_file() {
            continue;
        }
        let name = target.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let link = lnk.to_string_lossy().into_owned();
        // SAFETY: item creation from a path that exists.
        let icon = unsafe { SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&HSTRING::from(link.as_str()), None) }
            .ok()
            .and_then(|f| image(&f, icon_px));
        out.push(Recent { name, link, icon, when });
    }
    out
}

fn target(lnk: &Path) -> Option<PathBuf> {
    // SAFETY: loads a shortcut and reads its target path into a local buffer.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let w = HSTRING::from(lnk.to_string_lossy().as_ref());
        file.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        (n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}
