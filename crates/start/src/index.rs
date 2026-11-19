//! Files and folders from the Windows Search index: SQL from `ISearchQueryHelper` (catalog
//! SystemIndex), run through the OLE DB provider on a worker thread of its own. Each query
//! streams its rows to the UI thread in small batches; a newer query cancels the older one.

use crate::search;
use crate::{apps, log};
use shelldata::Image;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Search::{
    CSearchManager, DBACCESSOR_ROWDATA, DBBINDING, DBMEMOWNER_CLIENTOWNED, DBPARAMIO_NOTPARAM, DBPART_LENGTH, DBPART_STATUS,
    DBPART_VALUE, DBSTATUS_S_OK, DBTYPE_WSTR, HACCESSOR, IAccessor, ICommandText, IDBCreateCommand, IDBCreateSession,
    IDBInitialize, IDataInitialize, IRowset, ISearchManager, ISearchQueryHelper, MSDAINITIALIZE, SEARCH_TERM_PREFIX_ALL,
};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows_core::{GUID, HSTRING, IUnknown, Interface};

/// Posted to the menu window: lParam is a `Box<Batch>`.
pub const WM_FILES: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

const DBGUID_DEFAULT: GUID = GUID::from_u128(0xc8b521fb_5cf3_11ce_ade5_00aa0044773d);
const COLS: usize = 4;
const CHARS: usize = 300;
const MAX: i32 = 24;

pub struct FileHit {
    pub path: String,
    pub name: String,
    pub type_text: String,
    pub folder: bool,
    pub icon: Option<Image>,
}

pub struct Batch {
    pub generation: u64,
    pub hits: Vec<FileHit>,
    pub done: bool,
    pub ms: f32,
}

pub struct Index {
    tx: Sender<(u64, String)>,
}

impl Index {
    pub fn start(hwnd: HWND, icon_px: i32) -> Index {
        let (tx, rx) = channel();
        let target = hwnd.0 as isize;
        let _ = std::thread::Builder::new().name("start-index".into()).spawn(move || worker(rx, HWND(target as _), icon_px));
        Index { tx }
    }

    /// Asks for the files of `query`; an empty query only cancels.
    pub fn query(&self, generation: u64, query: &str) {
        let _ = self.tx.send((generation, query.to_string()));
    }
}

struct Db {
    helper: ISearchQueryHelper,
    session: IDBCreateCommand,
}

fn open() -> windows_core::Result<Db> {
    // SAFETY: COM creation and calls on this worker's apartment.
    unsafe {
        let mgr: ISearchManager = CoCreateInstance(&CSearchManager, None, CLSCTX_LOCAL_SERVER)?;
        let helper = mgr.GetCatalog(&HSTRING::from("SystemIndex"))?.GetQueryHelper()?;
        helper.SetQuerySelectColumns(&HSTRING::from("System.ItemPathDisplay,System.ItemNameDisplay,System.ItemType,System.ItemTypeText"))?;
        helper.SetQueryWhereRestrictions(&HSTRING::from("AND SCOPE='file:'"))?;
        helper.SetQueryContentProperties(&HSTRING::from("System.ItemNameDisplay"))?;
        helper.SetQueryTermExpansion(SEARCH_TERM_PREFIX_ALL)?;
        helper.SetQuerySorting(&HSTRING::from("System.Search.Rank DESC, System.DateModified DESC"))?;
        helper.SetQueryMaxResults(MAX)?;
        let conn = apps::take_pwstr(helper.ConnectionString()?);
        let init: IDataInitialize = CoCreateInstance(&MSDAINITIALIZE, None, CLSCTX_INPROC_SERVER)?;
        let mut ds: Option<IUnknown> = None;
        init.GetDataSource(None, CLSCTX_INPROC_SERVER.0, &HSTRING::from(conn.as_str()), &IDBInitialize::IID, &mut ds)?;
        let dbi: IDBInitialize = ds.ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_FAIL))?.cast()?;
        dbi.Initialize()?;
        let cs: IDBCreateSession = dbi.cast()?;
        let session: IDBCreateCommand = cs.CreateSession(None, &IDBCreateCommand::IID)?.cast()?;
        log::line(&format!("index: SystemIndex open, {conn}"));
        Ok(Db { helper, session })
    }
}

fn worker(rx: Receiver<(u64, String)>, hwnd: HWND, icon_px: i32) {
    // SAFETY: COM for this worker thread, for its whole life.
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let mut db: Option<Db> = None;
    let mut pending: Option<(u64, String)> = None;
    loop {
        let Some(mut job) = pending.take().or_else(|| rx.recv().ok()) else { break };
        // Only the newest query matters.
        while let Ok(j) = rx.try_recv() {
            job = j;
        }
        let (generation, q) = job;
        if q.trim().is_empty() {
            continue;
        }
        if db.is_none() {
            match open() {
                Ok(d) => db = Some(d),
                Err(e) => {
                    log::line(&format!("index: not available: {e}"));
                    post(hwnd, Batch { generation, hits: Vec::new(), done: true, ms: 0.0 });
                    continue;
                }
            }
        }
        let t = Instant::now();
        let d = db.as_ref().unwrap();
        let r = run_query(d, &q, generation, hwnd, icon_px, &rx, t);
        match r {
            Ok((n, next)) => {
                pending = next;
                log::line(&format!("index: \"{q}\": {n} files in {:.1} ms{}", t.elapsed().as_secs_f32() * 1000.0, if pending.is_some() { ", cut by a newer query" } else { "" }));
            }
            Err(e) => {
                log::line(&format!("index: \"{q}\" failed: {e}"));
                post(hwnd, Batch { generation, hits: Vec::new(), done: true, ms: t.elapsed().as_secs_f32() * 1000.0 });
            }
        }
    }
}

fn post(hwnd: HWND, b: Batch) {
    let p = Box::into_raw(Box::new(b));
    // SAFETY: hands the box to the UI thread, freed here when the post fails.
    unsafe {
        if PostMessageW(Some(hwnd), WM_FILES, WPARAM(0), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}

#[repr(C)]
struct Cell {
    status: u32,
    len: usize,
    value: [u16; CHARS],
}

fn sql_for(d: &Db, q: &str) -> String {
    // SAFETY: the helper builds SQL from the user's words.
    match unsafe { d.helper.GenerateSQLFromUserQuery(&HSTRING::from(q)) } {
        Ok(p) => apps::take_pwstr(p),
        Err(_) => search::index_sql(q, MAX as u32).unwrap_or_default(),
    }
}

fn run_query(d: &Db, q: &str, generation: u64, hwnd: HWND, icon_px: i32, rx: &Receiver<(u64, String)>, t: Instant) -> windows_core::Result<(usize, Option<(u64, String)>)> {
    let sql = sql_for(d, q);
    // SAFETY: OLE DB calls with bindings into a buffer that outlives every GetData.
    unsafe {
        let cmd: ICommandText = d.session.CreateCommand(None, &ICommandText::IID)?.cast()?;
        cmd.SetCommandText(&DBGUID_DEFAULT, &HSTRING::from(sql.as_str()))?;
        let mut rs: Option<IUnknown> = None;
        cmd.Execute(None, &IRowset::IID, None, None, Some(&mut rs))?;
        let rowset: IRowset = rs.ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_FAIL))?.cast()?;
        let acc: IAccessor = rowset.cast()?;
        let stride = size_of::<Cell>();
        let binds: Vec<DBBINDING> = (0..COLS)
            .map(|c| DBBINDING {
                iOrdinal: c + 1,
                obValue: c * stride + std::mem::offset_of!(Cell, value),
                obLength: c * stride + std::mem::offset_of!(Cell, len),
                obStatus: c * stride + std::mem::offset_of!(Cell, status),
                dwPart: (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32,
                dwMemOwner: DBMEMOWNER_CLIENTOWNED.0 as u32,
                eParamIO: DBPARAMIO_NOTPARAM.0 as u32,
                cbMaxLen: CHARS * 2,
                wType: DBTYPE_WSTR.0 as u16,
                ..Default::default()
            })
            .collect();
        let mut hacc = HACCESSOR::default();
        acc.CreateAccessor(DBACCESSOR_ROWDATA.0 as u32, COLS, binds.as_ptr(), COLS * stride, &mut hacc, None)?;
        let mut cells: Vec<Cell> = (0..COLS).map(|_| Cell { status: 0, len: 0, value: [0; CHARS] }).collect();
        let mut total = 0;
        let mut first = true;
        let mut newer = None;
        'rows: loop {
            let mut handles = [0usize; 8];
            let mut ptr = handles.as_mut_ptr();
            let mut got = 0usize;
            let hr = (rowset.vtable().GetNextRows)(rowset.as_raw(), 0, 0, handles.len() as isize, &mut got, &mut ptr);
            if hr.is_err() || got == 0 {
                break;
            }
            let mut hits = Vec::new();
            for &h in &handles[..got] {
                if rowset.GetData(h, hacc, cells.as_mut_ptr().cast()).is_err() {
                    continue;
                }
                let col = |c: &Cell| {
                    if c.status != DBSTATUS_S_OK.0 as u32 {
                        return String::new();
                    }
                    let n = (c.len / 2).min(CHARS);
                    String::from_utf16_lossy(&c.value[..n]).trim_end_matches('\0').to_string()
                };
                let path = col(&cells[0]);
                if path.is_empty() {
                    continue;
                }
                let item_type = col(&cells[2]);
                let folder = item_type.eq_ignore_ascii_case("Directory") || item_type.eq_ignore_ascii_case("Folder");
                let icon = SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&HSTRING::from(path.as_str()), None)
                    .ok()
                    .and_then(|f| apps::image(&f, icon_px));
                hits.push(FileHit { name: col(&cells[1]), type_text: col(&cells[3]), path, folder, icon });
            }
            let _ = rowset.ReleaseRows(got, handles.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
            total += hits.len();
            if first {
                log::line(&format!("index: \"{q}\": first {} rows at {:.1} ms", hits.len(), t.elapsed().as_secs_f32() * 1000.0));
                first = false;
            }
            post(hwnd, Batch { generation, hits, done: false, ms: t.elapsed().as_secs_f32() * 1000.0 });
            // A newer query is waiting: stop streaming this one.
            if let Ok(next) = rx.try_recv() {
                newer = Some(next);
                break 'rows;
            }
        }
        let _ = acc.ReleaseAccessor(hacc, None);
        post(hwnd, Batch { generation, hits: Vec::new(), done: true, ms: t.elapsed().as_secs_f32() * 1000.0 });
        Ok((total, newer))
    }
}
