//! Settings' XAML tree and the values this TAP owns. Tree notifications keep live handles so
//! a config edit can restyle Settings without advising the diagnostics service again.

use super::abi::*;
use super::backdrop;
use super::property;
use super::report;
use super::rules::Rules;
use super::state::{Background, Backgrounds};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use windows::Win32::Foundation::{E_FAIL, ERROR_NOT_FOUND};
use windows_core::{HRESULT, IInspectable, IUnknown, Interface, Weak, implement};

const DUMP_MAX: usize = 6000;
const BATCH: usize = 60;

#[implement(IVisualTreeServiceCallback2)]
pub struct Watch {
    tree: Arc<Tree>,
}

impl Watch {
    pub fn new(tree: Arc<Tree>) -> Self {
        Self { tree }
    }
}

pub struct Tree {
    svc: IVisualTreeService,
    diag: IXamlDiagnostics,
    t0: Instant,
    state: Mutex<State>,
}

// SAFETY: the diagnostics interfaces are free-threaded. Element and backdrop operations are
// only performed by the tree callbacks and the diagnostics UI dispatcher. The worker flushes logs.
unsafe impl Send for Tree {}
unsafe impl Sync for Tree {}

struct State {
    rules: Option<Arc<Rules>>,
    nodes: HashMap<InstanceHandle, Node>,
    sequence: usize,
    dumped: usize,
    buf: Vec<String>,
    brushes: HashMap<String, (InstanceHandle, Weak<IInspectable>)>,
    backgrounds: Backgrounds<Weak<IInspectable>>,
    windows: HashMap<InstanceHandle, backdrop::Kept>,
}

#[derive(Clone)]
struct Node {
    ty: String,
    name: String,
    children: u32,
    depth: u32,
    sequence: usize,
}

impl Node {
    fn label(&self) -> String {
        let short = self.ty.rsplit('.').next().unwrap_or(&self.ty);
        if self.name.is_empty() {
            short.into()
        } else {
            format!("{short}#{}", self.name)
        }
    }
}

impl Tree {
    pub fn new(svc: IVisualTreeService, diag: IXamlDiagnostics, t0: Instant) -> Self {
        Self {
            svc,
            diag,
            t0,
            state: Mutex::new(State {
                rules: None,
                nodes: HashMap::new(),
                sequence: 0,
                dumped: 0,
                buf: Vec::new(),
                brushes: HashMap::new(),
                backgrounds: Backgrounds(HashMap::new()),
                windows: HashMap::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn flush(&self) {
        let lines = std::mem::take(&mut self.state().buf);
        report::send(&lines);
    }

    pub fn pause(&self) {
        let mut state = self.state();
        state.rules = None;
        for kept in state.windows.values() {
            kept.pause();
        }
    }

    /// Called on the UI thread. Keep any values whose restoration failed so the worker can retry.
    /// Without a desired style this is the final rollback of an unadvised watch.
    pub fn update(&self, rules: Option<&Arc<Rules>>, owner_alive: impl FnOnce() -> bool) -> bool {
        self.pause();
        if !self.restore() {
            return false;
        }
        self.state().brushes.clear();
        if let Some(rules) = rules {
            let mut nodes = {
                let mut state = self.state();
                if !rules.idle() && !owner_alive() {
                    return true;
                }
                state.dumped = 0;
                state.rules = Some(rules.clone());
                state.nodes.iter().map(|(&h, n)| (h, n.clone())).collect::<Vec<_>>()
            };
            nodes.sort_unstable_by_key(|(_, node)| node.sequence);
            for (h, node) in nodes {
                let current = self.state();
                let apply = current.rules.as_ref().is_some_and(|active| Arc::ptr_eq(active, rules))
                    && current.nodes.get(&h).is_some_and(|n| n.sequence == node.sequence);
                drop(current);
                if apply {
                    self.apply(h, &node, rules);
                }
            }
            self.flush();
            report::line(format!("tap: live rules now {}, dump {}, pid {}", rules.len(), rules.dump, std::process::id()));
        }
        true
    }

    fn restore(&self) -> bool {
        let backgrounds = self.state().backgrounds.0.clone();
        let mut cleared = 0;
        for (identity, owned) in backgrounds {
            match self.clear_background(&owned) {
                Ok(did_clear) => {
                    cleared += usize::from(did_clear);
                    self.state().backgrounds.0.remove(&identity);
                }
                Err(e) => report::line(format!("tap: Background restore failed {:#010x}, will retry", e.0)),
            }
        }
        // Do not hold a mutex across calls into XAML, which may invoke tree callbacks.
        let windows = std::mem::take(&mut self.state().windows);
        for (h, mut kept) in windows {
            match backdrop::restore(&mut kept) {
                Ok(line) => report::line(line),
                Err(e) => {
                    self.state().windows.insert(h, kept);
                    report::line(format!("tap: system backdrop restore failed, will retry: {e}"));
                }
            }
        }
        let state = self.state();
        let complete = state.backgrounds.0.is_empty() && state.windows.is_empty();
        drop(state);
        report::line(format!("tap: {cleared} backgrounds cleared, rollback complete {complete}"));
        complete
    }

    fn clear_background(&self, owned: &Background<Weak<IInspectable>>) -> Result<bool, HRESULT> {
        let Some(element) = owned.element.upgrade() else {
            return Ok(false);
        };
        let mut handle = 0;
        // SAFETY: the live object and its out handle remain valid through both calls.
        unsafe {
            if self.diag.GetHandleFromIInspectable(element.as_raw(), &mut handle).is_ok()
                && handle != 0
                && let Some(cleared) = self.clear_at(handle, owned)?
            {
                return Ok(cleared);
            }
            // Detached elements can be missing from the diagnostics cache while still alive.
            self.diag.RegisterInstance(element.as_raw(), &mut handle).ok().map_err(|e| e.code())?;
        }
        self.clear_at(handle, owned)?.ok_or(ERROR_NOT_FOUND.to_hresult())
    }

    fn clear_at(&self, h: InstanceHandle, owned: &Background<Weak<IInspectable>>) -> Result<Option<bool>, HRESULT> {
        let brush = owned.brush.upgrade();
        let owns = |value| self.same_brush(value, brush.as_ref());
        let Some(bg) = property::get(&self.svc, h, "Background", Some(&owns))? else {
            return Ok(None);
        };
        if bg.value.index != owned.index || !bg.ownership.can_clear() {
            return Ok(Some(false));
        }
        // SAFETY: the property chain still contains the local brush we set.
        let hr = unsafe { self.svc.ClearProperty(h, owned.index) };
        if hr == ERROR_NOT_FOUND.to_hresult() {
            return Ok(None);
        }
        hr.ok().map_err(|e| e.code())?;
        Ok(Some(true))
    }

    fn added(&self, rel: &ParentChildRelation, el: &VisualElement) {
        let (node, rules) = {
            let mut state = self.state();
            if state.nodes.contains_key(&el.Handle) {
                return;
            }
            let depth = state.nodes.get(&rel.Parent).map_or(0, |parent| parent.depth + 1);
            let node = Node {
                ty: bstr(el.Type),
                name: bstr(el.Name),
                children: el.NumChildren,
                depth,
                sequence: state.sequence,
            };
            state.sequence += 1;
            state.nodes.insert(el.Handle, node.clone());
            (node, state.rules.clone())
        };
        if let Some(rules) = rules {
            self.apply(el.Handle, &node, &rules);
        }
    }

    fn apply(&self, h: InstanceHandle, node: &Node, rules: &Rules) {
        let label = node.label();
        if rules.dump && self.state().dumped < DUMP_MAX {
            let bg = property::get(&self.svc, h, "Background", None)
                .ok()
                .flatten()
                .map_or(String::new(), |p| format!(" bg={}", property::describe(&self.svc, &p.value)));
            let line = format!("tree {}{label} [{}]{bg}", " ".repeat(node.depth.min(60) as usize), node.children);
            let mut state = self.state();
            state.dumped += 1;
            state.buf.push(line);
            if state.buf.len() >= BATCH {
                let out = std::mem::take(&mut state.buf);
                drop(state);
                report::send(&out);
            }
        }
        if node.ty == "Windows.UI.Xaml.Window"
            && let Some(what) = &rules.backdrop
            && !self.state().windows.contains_key(&h)
        {
            match backdrop::apply(&self.diag, h, what) {
                Ok((message, kept)) => {
                    self.state().windows.insert(h, kept);
                    report::line(format!("{label}: system backdrop {message}"));
                }
                Err(e) => report::line(format!("{label}: system backdrop not set: {e}")),
            }
        }
        if let Some(color) = rules.color(&node.ty, &node.name) {
            match self.set_background(h, color) {
                Ok((i, reused)) => report::line(format!(
                    "{label}: Background {color} (property {i}{}) {} ms after advise",
                    if reused { ", recycled element kept our brush" } else { "" },
                    self.t0.elapsed().as_millis()
                )),
                Err(e) => report::line(format!("{label}: Background not set: {e}")),
            }
        }
    }

    fn removed(&self, el: &VisualElement) {
        // Detach can be temporary. Keep ownership by the live object's identity.
        self.state().nodes.remove(&el.Handle);
    }

    fn set_background(&self, h: InstanceHandle, color: &str) -> Result<(u32, bool), String> {
        let element = self.inspectable(h).map_err(|hr| format!("GetIInspectableFromHandle {:#010x}", hr.0))?;
        let unknown = element.cast::<IUnknown>().map_err(|e| format!("element identity: {e}"))?;
        let identity = unknown.as_raw() as usize;
        let owned = self.state().backgrounds.0.get(&identity).cloned()
            .filter(|owned| owned.element.upgrade().is_some());
        let owned_brush = owned.as_ref().and_then(|owned| owned.brush.upgrade());
        let owns = |value| self.same_brush(value, owned_brush.as_ref());
        let bg = property::get(&self.svc, h, "Background", Some(&owns))
            .map_err(|hr| format!("GetPropertyValuesChain {:#010x}", hr.0))?
            .ok_or("no Background property")?;
        if let Some(owned) = owned
            && bg.value.index == owned.index
            && bg.ownership.can_clear()
        {
            return Ok((owned.index, true));
        }
        if !bg.ownership.can_set() {
            return Err("local value or binding left alone".into());
        }
        let element = element.downgrade().map_err(|e| format!("element weak reference: {e}"))?;
        if element.upgrade().is_none() {
            return Err("element weak reference expired".into());
        }
        let (brush_handle, brush) = self.brush(color)?;
        // SAFETY: handles from this service; the index is from the element's property chain.
        let hr = unsafe { self.svc.SetProperty(h, brush_handle, bg.value.index) };
        if hr.is_err() {
            return Err(format!("SetProperty {:#010x}", hr.0));
        }
        let mut state = self.state();
        let backgrounds = &mut state.backgrounds;
        let count = backgrounds.0.len();
        if count > 0 && count.is_multiple_of(64) {
            backgrounds.prune(|weak| weak.upgrade().is_some());
        }
        backgrounds.0.insert(identity, Background { element, index: bg.value.index, brush });
        Ok((bg.value.index, false))
    }

    fn inspectable(&self, handle: InstanceHandle) -> Result<IInspectable, HRESULT> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the out pointer receives an owned IInspectable reference on success.
        unsafe {
            self.diag.GetIInspectableFromHandle(handle, &mut raw).ok().map_err(|e| e.code())?;
            if raw.is_null() { Err(E_FAIL) } else { Ok(IInspectable::from_raw(raw)) }
        }
    }

    fn same_brush(&self, handle: InstanceHandle, owned: Option<&IInspectable>) -> Result<bool, HRESULT> {
        let Some(owned) = owned else { return Ok(false) };
        Ok(self.inspectable(handle)? == *owned)
    }

    fn brush(&self, color: &str) -> Result<(InstanceHandle, Weak<IInspectable>), String> {
        if let Some(b) = self.state().brushes.get(color).cloned() {
            return Ok(b);
        }
        let ty = Bstr::new("Windows.UI.Xaml.Media.SolidColorBrush");
        let v = Bstr::new(color);
        let mut b = 0;
        // SAFETY: two live BSTRs and an out handle.
        let hr = unsafe { self.svc.CreateInstance(ty.0, v.0, &mut b) };
        if hr.is_err() || b == 0 {
            return Err(format!("CreateInstance(SolidColorBrush {color}) {:#010x}", hr.0));
        }
        let brush = self.inspectable(b).map_err(|hr| format!("brush instance {:#010x}", hr.0))?;
        let weak = brush.downgrade().map_err(|e| format!("brush weak reference: {e}"))?;
        if weak.upgrade().is_none() {
            return Err("brush weak reference expired".into());
        }
        let cached = (b, weak);
        self.state().brushes.insert(color.into(), cached.clone());
        Ok(cached)
    }
}

impl IVisualTreeServiceCallback_Impl for Watch_Impl {
    unsafe fn OnVisualTreeChange(&self, relation: ParentChildRelation, element: VisualElement, mutation: VisualMutationType) -> HRESULT {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if mutation == ADD {
                self.tree.added(&relation, &element);
            } else {
                self.tree.removed(&element);
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
