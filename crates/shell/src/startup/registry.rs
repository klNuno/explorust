use crate::startup_rules;
use windows::Win32::{
    Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
    System::{
        Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_CREATE_SUB_KEY, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_32KEY,
            KEY_WOW64_64KEY, REG_CREATED_NEW_KEY, REG_EXPAND_SZ, REG_OPTION_VOLATILE,
            REG_SAM_FLAGS, REG_SZ, RRF_RT_REG_BINARY, RegCloseKey, RegCreateKeyExW,
            REG_VALUE_TYPE, RegDeleteValueW, RegEnumValueW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
        },
        RemoteDesktop::ProcessIdToSessionId,
    },
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

const VERSION: &str = r"Software\Microsoft\Windows\CurrentVersion";
pub type View = REG_SAM_FLAGS;
pub const NATIVE: View = KEY_WOW64_64KEY;
pub const WOW32: View = KEY_WOW64_32KEY;

pub fn version_path(name: &str) -> HSTRING {
    HSTRING::from(format!(r"{VERSION}\{name}"))
}

/// Explorer 26300 opens a session key and uses RegCreateKeyExW(REG_OPTION_VOLATILE) on
/// RunStuffHasBeenRun. The disposition tells it whether this is the first launch of the session.
/// Use the same key so an explorust restart or handoff to Explorer cannot launch the apps again.
pub fn claim() -> windows::core::Result<bool> {
    let mut session = 0;
    // SAFETY: session is a writable DWORD; create_key owns the returned HKEY.
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session)? };
    let window_session = format!(r"{VERSION}\Explorer\SessionInfo\{session}");
    let (key, _) = create_key(HKEY_CURRENT_USER, &window_session, KEY_CREATE_SUB_KEY)?;
    let (_, first) = create_key(key.0, "RunStuffHasBeenRun", KEY_CREATE_SUB_KEY)?;
    Ok(first)
}

fn create_key(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> windows::core::Result<(Key, bool)> {
    let mut raw = HKEY::default();
    let mut created = Default::default();
    // SAFETY: registry path and outputs are live through the call. RegCloseKey balances it.
    unsafe {
        RegCreateKeyExW(
            root,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_VOLATILE,
            access,
            None,
            &mut raw,
            Some(&mut created),
        )
    }
    .ok()?;
    Ok((Key(raw), created == REG_CREATED_NEW_KEY))
}

pub struct Key(HKEY);

// SAFETY: the registry API accepts an open handle from any thread. Moving the owner
// also moves responsibility for closing it.
unsafe impl Send for Key {}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns an opened handle.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

impl Key {
    pub fn open(root: HKEY, path: &HSTRING, access: REG_SAM_FLAGS) -> Option<Self> {
        let mut raw = HKEY::default();
        // SAFETY: the output and HSTRING are valid for the call.
        unsafe { RegOpenKeyExW(root, path, None, access, &mut raw) }
            .ok()
            .ok()?;
        Some(Self(raw))
    }

    /// Snapshot the values before deleting RunOnce entries, which changes their indices.
    pub fn values(&self) -> Vec<Value> {
        let (mut names, mut bytes, mut count) = (0, 0, 0);
        // SAFETY: the three output sizes are DWORDs owned here.
        if unsafe {
            RegQueryInfoKeyW(
                self.0,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&mut count),
                Some(&mut names),
                Some(&mut bytes),
                None,
                None,
            )
        } != ERROR_SUCCESS
        {
            return vec![];
        }
        let mut name = vec![0u16; names as usize + 1];
        let mut data = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
        let mut values = Vec::new();
        for index in 0..count {
            let mut kind = 0;
            let mut name_len = name.len() as u32;
            let mut data_len = (data.len() * 2) as u32;
            // SAFETY: the lengths describe these buffers, with data size in bytes.
            let rc = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    Some(&mut kind),
                    Some(data.as_mut_ptr().cast()),
                    Some(&mut data_len),
                )
            };
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            if rc == ERROR_SUCCESS
                && (kind == REG_SZ.0 || kind == REG_EXPAND_SZ.0)
                && data_len % 2 == 0
            {
                values.push(Value {
                    name: HSTRING::from_wide(&name[..name_len as usize]),
                    data: data[..data_len as usize / 2].to_vec(),
                    kind,
                });
            }
        }
        values
    }
}

pub struct Value {
    pub name: HSTRING,
    pub data: Vec<u16>,
    pub kind: u32,
}

impl Key {
    pub fn writer(root: HKEY, path: &HSTRING, view: View) -> windows::core::Result<Self> {
        let mut raw = HKEY::default();
        // SAFETY: the output and HSTRING are valid for the call.
        unsafe { RegOpenKeyExW(root, path, None, KEY_QUERY_VALUE | KEY_SET_VALUE | view, &mut raw) }.ok()?;
        Ok(Self(raw))
    }

    pub fn delete(&self, name: &HSTRING) -> windows::core::Result<()> {
        // SAFETY: the open key handle and value name outlive the operation.
        unsafe { RegDeleteValueW(self.0, name) }.ok()
    }

    pub fn matches(&self, value: &Value) -> bool {
        let mut data = vec![0u16; value.data.len()];
        let mut bytes = size_of_val(data.as_slice()) as u32;
        let mut kind = REG_VALUE_TYPE::default();
        // SAFETY: RegQueryValueExW reads the exact bytes without expanding or adding a NUL.
        let result = unsafe {
            RegQueryValueExW(self.0, &value.name, None, Some(&mut kind), Some(data.as_mut_ptr().cast()), Some(&mut bytes))
        };
        result == ERROR_SUCCESS && bytes as usize == size_of_val(data.as_slice())
            && startup_rules::same_value((value.kind, &value.data), (kind.0, &data))
    }
}

pub fn disabled(root: HKEY, subkey: &str, name: &HSTRING) -> bool {
    let path = version_path(&format!(r"Explorer\StartupApproved\{subkey}"));
    let mut data = [0u8; 12];
    let mut size = data.len() as u32;
    // SAFETY: the size matches the buffer.
    let rc = unsafe {
        RegGetValueW(
            root,
            &path,
            name,
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    rc == ERROR_SUCCESS && startup_rules::disabled(&data[..size as usize])
}
