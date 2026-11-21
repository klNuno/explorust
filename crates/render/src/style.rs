//! Per-user materials shared by Settings and the shell's composition surfaces.

use serde_json::{Map, Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Taskbar,
    Start,
    Notifications,
    QuickSettings,
    Files,
    Menus,
    Switcher,
}

impl Part {
    pub const ALL: [Self; 7] = [
        Self::Taskbar,
        Self::Start,
        Self::Notifications,
        Self::QuickSettings,
        Self::Files,
        Self::Menus,
        Self::Switcher,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Taskbar => "taskbar",
            Self::Start => "start",
            Self::Notifications => "notifications",
            Self::QuickSettings => "quick_settings",
            Self::Files => "files",
            Self::Menus => "menus",
            Self::Switcher => "alt_tab",
        }
    }

    pub const fn defaults(self) -> Style {
        let (material, opacity) = match self {
            Self::Taskbar => (Material::Transparent, 12),
            Self::Start | Self::Notifications | Self::QuickSettings => (Material::Acrylic, 58),
            Self::Files => (Material::Mica, 72),
            Self::Menus | Self::Switcher => (Material::Windows, 72),
        };
        Style { material, opacity }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    Windows,
    Acrylic,
    Transparent,
    Mica,
    Oled,
}

impl Material {
    pub const ALL: [Self; 5] = [
        Self::Windows,
        Self::Acrylic,
        Self::Transparent,
        Self::Mica,
        Self::Oled,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Acrylic => "acrylic",
            Self::Transparent => "transparent",
            Self::Mica => "mica",
            Self::Oled => "oled",
        }
    }

    pub fn can_tint(self) -> bool {
        matches!(self, Self::Acrylic | Self::Transparent)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub material: Material,
    pub opacity: u8,
}

#[derive(Clone, Debug)]
pub struct Document(Map<String, Value>);

impl Default for Document {
    fn default() -> Self {
        Self(json!({ "version": 1 }).as_object().unwrap().clone())
    }
}

impl Document {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(bytes)
            .map(Self)
            .map_err(io::Error::other)
    }

    pub fn load() -> io::Result<Self> {
        match fs::read(path()?) {
            Ok(bytes) => Self::parse(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn get(&self, part: Part) -> Style {
        let defaults = part.defaults();
        let Some(value) = self.0.get(part.key()) else {
            return defaults;
        };
        let material = value
            .get("material")
            .and_then(Value::as_str)
            .and_then(|key| Material::ALL.into_iter().find(|m| m.key() == key))
            .unwrap_or(defaults.material);
        let opacity = value
            .get("opacity")
            .and_then(Value::as_u64)
            .map(|n| n.min(100) as u8)
            .unwrap_or(defaults.opacity);
        Style { material, opacity }
    }

    pub fn set(&mut self, part: Part, style: Style) {
        let entry = self.0.entry(part.key()).or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        entry["material"] = json!(style.material.key());
        entry["opacity"] = json!(style.opacity.min(100));
    }

    pub fn reset(&mut self, part: Part) {
        if let Some(value) = self.0.get_mut(part.key()).and_then(Value::as_object_mut) {
            value.remove("material");
            value.remove("opacity");
        }
    }

    pub fn save(&mut self) -> io::Result<()> {
        self.0.entry("version").or_insert_with(|| json!(1));
        let path = path()?;
        fs::create_dir_all(path.parent().unwrap())?;
        // Publish only a whole document. Distinct temp paths also let old readers finish.
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let tmp = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = (|| {
            serde_json::to_writer_pretty(&mut file, &self.0).map_err(io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

fn path() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("explorust").join("style.json"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Local AppData is unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_a_material_preserves_extensions_and_the_loaded_version() {
        let mut doc = Document::parse(
            br#"{"version":3,"extra":true,"files":{"future":6,"material":"oled","opacity":120}}"#,
        )
        .unwrap();
        assert_eq!(
            doc.get(Part::Files),
            Style {
                material: Material::Oled,
                opacity: 100
            }
        );
        doc.set(
            Part::Files,
            Style {
                material: Material::Mica,
                opacity: 61,
            },
        );
        assert_eq!(doc.0["version"], 3);
        assert_eq!(doc.0["extra"], true);
        assert_eq!(doc.0["files"]["future"], 6);
        doc.reset(Part::Files);
        assert_eq!(doc.0["files"], json!({"future": 6}));
        assert_eq!(doc.get(Part::Files), Part::Files.defaults());
    }

    #[test]
    fn missing_or_unknown_material_uses_the_surface_default() {
        let doc = Document::parse(br#"{"start":{"material":"future"},"taskbar":{"opacity":-1}}"#)
            .unwrap();
        for part in Part::ALL {
            assert_eq!(doc.get(part), part.defaults());
        }
    }
}
