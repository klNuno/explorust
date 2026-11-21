//! The TAP's tree watch: the diagnostics service reports every element of Settings' visual tree
//! (the existing ones first, then each one added); an element a rule names gets a brush of the
//! rule's color (transparent by default) as its local `Background`, so the backdrop explorust gives
//! Settings' frame shows through.

use super::abi::*;
use super::report;
use super::rules::Rules;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows_core::{HRESULT, implement};

/// Tree lines sent by the dump at most.
const DUMP_MAX: usize = 6000;
/// Dump lines per `WM_COPYDATA`.
const BATCH: usize = 60;
/// `PropertyChainValue::MetadataBits`: the value is another element's handle, or null.
const IS_VALUE_HANDLE: i64 = 0x1;
const IS_VALUE_NULL: i64 = 0x20;

#[implement(IVisualTreeServiceCallback2)]
pub struct Watch {
    svc: IVisualTreeService,
    diag: IXamlDiagnostics,
    rules: Rules,
    t0: Instant,
    st: Arc<Mutex<State>>,
}

#[derive(Default)]
pub struct State {
    brushes: HashMap<String, InstanceHandle>,
    depth: HashMap<InstanceHandle, u32>,
    seen: usize,
    dumped: usize,
    cleared: usize,
    buf: Vec<String>,
}

/// An effective property value: its index in the property chain, its value as text (a
/// handle's number when `handle`) and the value's type.
struct Prop {
    index: u32,
    value: String,
    ty: String,
    handle: bool,
}

impl Watch {
    /// The watch and its state, which a timer thread flushes with [`flush`].
    pub fn new(svc: IVisualTreeService, diag: IXamlDiagnostics, rules: Rules, t0: Instant) -> (Watch, Arc<Mutex<State>>) {
        let st = Arc::new(Mutex::new(State::default()));
        (Watch { svc, diag, rules, t0, st: st.clone() }, st)
    }
}

fn lock(st: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    st.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sends what the dump still holds and a count (called from a timer thread after the first burst).
pub fn flush(st: &Mutex<State>, t0: Instant) {
    let (lines, seen, cleared) = {
        let mut st = lock(st);
        (std::mem::take(&mut st.buf), st.seen, st.cleared)
    };
    report::send(&lines);
    report::line(format!("tree so far: {seen} elements seen, {cleared} backgrounds set, {} ms after advise", t0.elapsed().as_millis()));
}

impl Watch_Impl {
    fn added(&self, rel: &ParentChildRelation, el: &VisualElement) {
        let ty = bstr(el.Type);
        let name = bstr(el.Name);
        let short = ty.rsplit('.').next().unwrap_or(&ty).to_string();
        let label = if name.is_empty() { short.clone() } else { format!("{short}#{name}") };
        let depth = {
            let mut st = lock(&self.st);
            let d = if rel.Parent == 0 { 0 } else { st.depth.get(&rel.Parent).map_or(0, |d| d + 1) };
            st.depth.insert(el.Handle, d);
            st.seen += 1;
            d
        };
        if self.rules.dump && lock(&self.st).dumped < DUMP_MAX {
            // The service is not called with the lock held: a call may report tree changes.
            let bg = match self.background(el.Handle) {
                Ok(Some(b)) => format!(" bg={}", self.describe(&b)),
                _ => String::new(),
            };
            let line = format!("tree {}{label} [{}]{bg}", " ".repeat(depth.min(60) as usize), el.NumChildren);
            let out = {
                let mut st = lock(&self.st);
                st.dumped += 1;
                st.buf.push(line);
                if st.buf.len() >= BATCH { std::mem::take(&mut st.buf) } else { Vec::new() }
            };
            report::send(&out);
        }
        if ty == "Windows.UI.Xaml.Window"
            && let Some(what) = &self.rules.backdrop
        {
            let t = Instant::now();
            let r = super::backdrop::apply(&self.diag, el.Handle, what);
            let us = t.elapsed().as_micros();
            match r {
                Ok(s) => report::line(format!("{label}: system backdrop {s} in {us} us, {} ms after advise", self.t0.elapsed().as_millis())),
                Err(e) => report::line(format!("{label}: system backdrop not set: {e}")),
            }
        }
        if let Some(color) = self.rules.color(&ty, &name) {
            let t = Instant::now();
            let r = self.set_background(el.Handle, color);
            let us = t.elapsed().as_micros();
            match r {
                Ok(i) => {
                    let n = {
                        let mut st = lock(&self.st);
                        st.cleared += 1;
                        st.cleared
                    };
                    report::line(format!(
                        "{label}: Background {color} (property {i}) in {us} us, {n} so far, {} ms after advise",
                        self.t0.elapsed().as_millis()
                    ));
                }
                Err(e) => report::line(format!("{label}: Background not set: {e}")),
            }
        }
    }

    fn removed(&self, el: &VisualElement) {
        lock(&self.st).depth.remove(&el.Handle);
    }

    /// Sets the element's local `Background` to a shared brush of that color; the property's index.
    fn set_background(&self, h: InstanceHandle, color: &str) -> Result<u32, String> {
        let brush = self.brush(color)?;
        let bg = self.background(h)?.ok_or("no Background property")?;
        // SAFETY: handles from this service; the index came from the element's own chain.
        let hr = unsafe { self.svc.SetProperty(h, brush, bg.index) };
        if hr.is_err() {
            return Err(format!("SetProperty {:#010x}", hr.0));
        }
        Ok(bg.index)
    }

    fn brush(&self, color: &str) -> Result<InstanceHandle, String> {
        if let Some(b) = lock(&self.st).brushes.get(color) {
            return Ok(*b);
        }
        let ty = Bstr::new("Windows.UI.Xaml.Media.SolidColorBrush");
        let v = Bstr::new(color);
        let mut b: InstanceHandle = 0;
        // SAFETY: two live BSTRs and an out handle.
        let hr = unsafe { self.svc.CreateInstance(ty.0, v.0, &mut b) };
        if hr.is_err() || b == 0 {
            return Err(format!("CreateInstance(SolidColorBrush {color}) {:#010x}", hr.0));
        }
        lock(&self.st).brushes.insert(color.to_string(), b);
        Ok(b)
    }

    /// A Background for the dump: `SolidColorBrush(#FFF3F3F3)`, `AcrylicBrush`, `null`.
    fn describe(&self, b: &Prop) -> String {
        let short = b.ty.rsplit('.').next().unwrap_or(&b.ty).to_string();
        if !b.handle {
            return if short.is_empty() { b.value.clone() } else { format!("{short}({})", b.value) };
        }
        let color = b.value.parse::<InstanceHandle>().ok().and_then(|h| self.property(h, "Color").ok().flatten());
        match color {
            Some(c) => format!("{short}({})", c.value),
            None => short,
        }
    }

    fn background(&self, h: InstanceHandle) -> Result<Option<Prop>, String> {
        self.property(h, "Background")
    }

    /// An element's effective property (the chain entry not overridden by another source), or None
    /// when it has no such property.
    fn property(&self, h: InstanceHandle, prop: &str) -> Result<Option<Prop>, String> {
        let (mut ns, mut np) = (0u32, 0u32);
        let mut sources: *mut PropertyChainSource = std::ptr::null_mut();
        let mut values: *mut PropertyChainValue = std::ptr::null_mut();
        // SAFETY: out pointers to locals; the arrays and their BSTRs are freed below.
        let hr = unsafe { self.svc.GetPropertyValuesChain(h, &mut ns, &mut sources, &mut np, &mut values) };
        if hr.is_err() {
            return Err(format!("GetPropertyValuesChain {:#010x}", hr.0));
        }
        let mut found: Option<(bool, Prop)> = None;
        // SAFETY: the service returned `np` values and `ns` sources in CoTaskMem arrays.
        unsafe {
            for i in 0..np as usize {
                let v = &*values.add(i);
                if bstr(v.PropertyName) == prop {
                    let effective = v.Overridden == 0;
                    if found.as_ref().is_none_or(|(e, _)| !e && effective) {
                        let null = v.MetadataBits & IS_VALUE_NULL != 0;
                        let value = if null { "null".to_string() } else { bstr(v.Value) };
                        let handle = !null && v.MetadataBits & IS_VALUE_HANDLE != 0;
                        found = Some((effective, Prop { index: v.Index, value, ty: bstr(v.ValueType), handle }));
                    }
                }
                for s in [v.Type, v.DeclaringType, v.ValueType, v.ItemType, v.Value, v.PropertyName] {
                    SysFreeString(s);
                }
            }
            for i in 0..ns as usize {
                let s = &*sources.add(i);
                SysFreeString(s.TargetType);
                SysFreeString(s.Name);
                SysFreeString(s.SrcInfo.FileName as *mut u16);
                SysFreeString(s.SrcInfo.Hash as *mut u16);
            }
            CoTaskMemFree(values as *const _);
            CoTaskMemFree(sources as *const _);
        }
        Ok(found.map(|(_, b)| b))
    }
}

impl IVisualTreeServiceCallback_Impl for Watch_Impl {
    unsafe fn OnVisualTreeChange(&self, relation: ParentChildRelation, element: VisualElement, mutation: VisualMutationType) -> HRESULT {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if mutation == ADD {
                self.added(&relation, &element);
            } else {
                self.removed(&element);
            }
        }));
        HRESULT(0)
    }
}

impl IVisualTreeServiceCallback2_Impl for Watch_Impl {
    unsafe fn OnElementStateChanged(&self, _element: InstanceHandle, _state: i32, _context: *const u16) -> HRESULT {
        HRESULT(0)
    }
}
