//! Details for the focused selection, from the native shell item's cached properties.

use crate::text::Strings;
use crate::win::gfx::{Canvas, rect};
use crate::win::shell::{self, Pidl};
use render::rgba;
use windows::Win32::Foundation::{PROPERTYKEY, RECT};
use windows::Win32::Storage::EnhancedStorage::{PKEY_DateModified, PKEY_ItemTypeText, PKEY_Size};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::PropertiesSystem::{GPS_FASTPROPERTIESONLY, IPropertyStore, PDFF_DEFAULT, PSFormatForDisplayAlloc};
use windows::Win32::UI::Shell::{IShellItem, IShellItem2, SIGDN_NORMALDISPLAY};
use windows_core::{Interface, Result};

pub struct Pane {
    pub canvas: Canvas,
    bounds: RECT,
    scale: f32,
    item: Option<(i32, Option<Pidl>)>,
}

impl Pane {
    pub fn new(parent: &Canvas, bounds: RECT, scale: f32) -> Result<Self> {
        let (w, h) = dimensions(bounds);
        let canvas = parent.add_layer(w, h, 96.0 * scale)?;
        if let Err(error) = canvas.set_origin(bounds.left, bounds.top) {
            let _ = parent.remove_layer(&canvas);
            return Err(error);
        }
        Ok(Self { canvas, bounds, scale, item: None })
    }

    pub fn place(&mut self, bounds: RECT, scale: f32) -> Result<()> {
        if self.bounds == bounds && self.scale == scale {
            return Ok(());
        }
        let (w, h) = dimensions(bounds);
        if dimensions(self.bounds) != (w, h) || scale != self.scale {
            self.canvas.resize(w, h, 96.0 * scale)?;
            self.item = None;
        }
        self.canvas.set_origin(bounds.left, bounds.top)?;
        self.bounds = bounds;
        self.scale = scale;
        Ok(())
    }

    pub fn draw(&mut self, item: Option<&IShellItem>, selected: i32, s: &Strings) -> Result<()> {
        let key = (selected, item.and_then(Pidl::of));
        if self.item.as_ref() == Some(&key) {
            return Ok(());
        }
        let c = &self.canvas;
        let w = dimensions(self.bounds).0 as f32 / self.scale;
        let h = dimensions(self.bounds).1 as f32 / self.scale;
        let text = rgba(240, 240, 240, 1.0);
        let dim = rgba(182, 182, 182, 1.0);
        c.begin();
        c.fill(rect(0.0, 0.0, w, h), rgba(31, 31, 31, 1.0));
        c.fill(rect(0.0, 0.0, 1.0, h), rgba(53, 53, 53, 1.0));
        let title = match item {
            Some(item) => shell::name(item, SIGDN_NORMALDISPLAY),
            None if selected > 1 => format!("{selected} {}", s.items_selected),
            None => s.details.into(),
        };
        c.text(&c.sh.fonts.ui14, &title, 18.0, w - 18.0, 35.0, text);
        if let Some(item) = item {
            let mut y = 67.0;
            if let Some(store) = cached_properties(item) {
                let properties = [PKEY_ItemTypeText, PKEY_Size, PKEY_DateModified];
                for (key, label) in properties.iter().zip(s.detail_labels) {
                    if let Some(value) = format_property(&store, key) {
                        c.text(&c.sh.fonts.ui12, label, 18.0, w - 18.0, y, dim);
                        c.text(&c.sh.fonts.ui14, &value, 18.0, w - 18.0, y + 20.0, text);
                        y += 55.0;
                    }
                }
            }
        }
        c.end()?;
        self.item = Some(key);
        Ok(())
    }
}

fn dimensions(bounds: RECT) -> (u32, u32) {
    ((bounds.right - bounds.left).max(1) as u32, (bounds.bottom - bounds.top).max(1) as u32)
}

fn cached_properties(item: &IShellItem) -> Option<IPropertyStore> {
    let keys = [PKEY_ItemTypeText, PKEY_Size, PKEY_DateModified];
    unsafe { item.cast::<IShellItem2>().ok()?.GetPropertyStoreForKeys(&keys, GPS_FASTPROPERTIESONLY).ok() }
}

fn format_property(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let value = store.GetValue(key).ok()?;
        let formatted = PSFormatForDisplayAlloc(key, &value, PDFF_DEFAULT).ok()?;
        if formatted.is_null() {
            return None;
        }
        let text = formatted.to_string().unwrap_or_default();
        CoTaskMemFree(Some(formatted.0.cast()));
        (!text.is_empty()).then_some(text)
    }
}
