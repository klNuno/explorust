//! Snapshots contain privileged filenames, so the cache directory and its children have a
//! protected DACL granting access only to SYSTEM and administrators.

use crate::log;
use fsindex::table::{Snapshot, SnapshotCursor, Table};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SetFileSecurityW,
};
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT};
use windows::Win32::System::Ioctl::USN_JOURNAL_DATA_V0;
use windows_core::{HSTRING, w};

static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

fn path() -> io::Result<PathBuf> {
    Ok(std::env::current_exe()?.with_file_name("index-cache"))
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: CreateDirectory copies the valid descriptor before LocalFree releases it.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            w!("O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        let result = match CreateDirectoryW(&wide, Some(&attributes)) {
            Err(e) if e.code() == ERROR_ALREADY_EXISTS.to_hresult() => fs::symlink_metadata(path)
                .and_then(|meta| {
                    if !meta.is_dir()
                        || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
                    {
                        return Err(e.into());
                    }
                    let flags = OWNER_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION
                        | PROTECTED_DACL_SECURITY_INFORMATION;
                    SetFileSecurityW(&wide, flags, descriptor)
                        .ok()
                        .map_err(io::Error::from)
                }),
            result => result.map_err(io::Error::from),
        };
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        result
    }
}

pub fn enable() {
    match path().and_then(|dir| {
        create_private_directory(&dir)?;
        Ok(dir)
    }) {
        Ok(dir) => {
            let _ = DIRECTORY.set(dir);
        }
        Err(e) => log::line(&format!("index: cannot open snapshot directory: {e}")),
    }
}

pub fn clear() -> io::Result<()> {
    match fs::remove_dir_all(path()?) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub struct Cache {
    file: PathBuf,
    temporary: PathBuf,
}

impl Cache {
    pub fn for_volume(device: &HSTRING, letter: char) -> Option<Self> {
        let dir = DIRECTORY.get()?;
        let device = device.to_string_lossy();
        let guid = device.strip_prefix(r"\\?\Volume{")?.strip_suffix('}')?;
        if guid.len() != 36 || !guid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return None;
        }
        Some(Self {
            file: dir.join(format!("{guid}.mft")),
            temporary: dir.join(format!("{guid}-{letter}.tmp")),
        })
    }

    pub fn read(&self, journal: &USN_JOURNAL_DATA_V0) -> io::Result<Option<Snapshot>> {
        let file = match File::open(&self.file) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let bytes = file.metadata()?.len();
        let low = journal.FirstUsn.max(journal.LowestValidUsn);
        Snapshot::read_if(BufReader::new(file), bytes, |cursor| {
            cursor.journal == journal.UsnJournalID && (low..=journal.NextUsn).contains(&cursor.next)
        })
    }

    pub fn save(
        &self,
        names: &Table,
        sequences: &[u16],
        pruned: &[u64],
        cursor: SnapshotCursor,
    ) -> io::Result<()> {
        let result = (|| {
            match fs::remove_file(&self.temporary) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            let temporary = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.temporary)?;
            let mut out = BufWriter::new(temporary);
            Snapshot::write(&mut out, names, cursor, sequences, pruned)?;
            let file = out.into_inner()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&self.temporary, &self.file)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&self.temporary);
        }
        result
    }
}
