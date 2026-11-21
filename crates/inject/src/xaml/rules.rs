//! What the TAP changes in Settings, from `%LOCALAPPDATA%\explorust\settings-style.txt`: plain
//! logic, tested on any OS. There are no built-in rules: without the file Settings is left alone.
//!
//! The TAP reads the rules from its configuration directory: one rule per line, `Type#Name`,
//! `Type` or `#Name`, optionally
//! followed by `= <color>` (a XAML color: `Transparent`, `#80202020`; Transparent when left out); a
//! comment starts with `# ` (hash, space), blank lines are skipped. A type matches the element's
//! full type name or its last segment (`Grid` matches `Windows.UI.Xaml.Controls.Grid`). A line
//! `!dump` makes the TAP send the visual tree, with each element's effective Background, to the
//! loader's log too. A line `!backdrop <what>` replaces the window's system backdrop (the Mica
//! brush Settings paints under everything): `none`, `host` (the blurred view behind the window,
//! where DWM can blur) or a color `#AARRGGBB`. With `none`, g1 shows a white layer under the XAML
//! even with the frame turned to glass from outside: the frame host's own surface, not the DWM
//! backdrop (xaml-1, session D).

/// `#AARRGGBB` or `#RRGGBB` as `[a, r, g, b]`.
pub fn argb(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    let v = u32::from_str_radix(h, 16).ok()?;
    match h.len() {
        8 => Some(v.to_be_bytes()),
        6 => Some((v | 0xff00_0000).to_be_bytes()),
        _ => None,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rules {
    pub dump: bool,
    pub backdrop: Option<String>,
    list: Vec<Rule>,
}

#[derive(Debug, PartialEq, Eq)]
struct Rule {
    ty: Option<String>,
    name: Option<String>,
    color: String,
}

impl Rules {
    pub fn parse(text: &str) -> Rules {
        let mut r = Rules::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with("# ") || line == "#" {
                continue;
            }
            if line == "!dump" {
                r.dump = true;
                continue;
            }
            if let Some(b) = line.strip_prefix("!backdrop") {
                let b = b.trim();
                r.backdrop = (!b.is_empty()).then(|| b.to_string());
                continue;
            }
            let (line, color) = match line.split_once('=') {
                Some((l, c)) if !c.trim().is_empty() => (l.trim(), c.trim()),
                Some((l, _)) => (l.trim(), "Transparent"),
                None => (line, "Transparent"),
            };
            let (ty, name) = match line.split_once('#') {
                Some((t, n)) => (t, n),
                None => (line, ""),
            };
            let some = |s: &str| (!s.is_empty()).then(|| s.to_string());
            if ty.is_empty() && name.is_empty() {
                continue;
            }
            r.list.push(Rule {
                ty: some(ty),
                name: some(name),
                color: color.to_string(),
            });
        }
        r
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// No style or tree dump is requested.
    pub fn idle(&self) -> bool {
        self.list.is_empty() && !self.dump && self.backdrop.is_none()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The Background color an element of this full type name and `x:Name` gets, from the first
    /// rule that matches it.
    pub fn color(&self, ty: &str, name: &str) -> Option<&str> {
        let short = ty.rsplit('.').next().unwrap_or(ty);
        self.list
            .iter()
            .find(|r| r.ty.as_deref().is_none_or(|t| t == ty || t == short) && r.name.as_deref().is_none_or(|n| n == name))
            .map(|r| r.color.as_str())
    }

    pub fn matches(&self, ty: &str, name: &str) -> bool {
        self.color(ty, name).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_name_and_both() {
        let r = Rules::parse("# comment\n\nGrid#ContentRoot\n#PaneRoot\nWindows.UI.Xaml.Controls.Border\n!dump\n");
        assert!(r.dump);
        assert_eq!(r.len(), 3);
        assert!(r.matches("Windows.UI.Xaml.Controls.Grid", "ContentRoot"));
        assert!(!r.matches("Windows.UI.Xaml.Controls.Grid", "Other"));
        assert!(r.matches("Microsoft.UI.Xaml.Controls.SplitView", "PaneRoot"));
        assert!(r.matches("Windows.UI.Xaml.Controls.Border", "anything"));
        assert!(!r.matches("My.Border", "x"));
        assert_eq!(r.color("Windows.UI.Xaml.Controls.Grid", "ContentRoot"), Some("Transparent"));
    }

    #[test]
    fn colors() {
        let r = Rules::parse("#RootPage = #40FF0000\nGrid#A=\nPage\n");
        assert_eq!(r.color("Windows.UI.Xaml.Controls.Page", "RootPage"), Some("#40FF0000"));
        assert_eq!(r.color("Windows.UI.Xaml.Controls.Grid", "A"), Some("Transparent"));
        assert_eq!(r.color("Windows.UI.Xaml.Controls.Page", "Other"), Some("Transparent"));
        assert_eq!(r.color("Windows.UI.Xaml.Controls.Grid", "B"), None);
    }

    #[test]
    fn colors_as_argb() {
        assert_eq!(argb("#40FF0080"), Some([0x40, 0xff, 0, 0x80]));
        assert_eq!(argb("#102030"), Some([0xff, 0x10, 0x20, 0x30]));
        assert_eq!(argb("red"), None);
        assert_eq!(argb("#12345"), None);
    }

    #[test]
    fn idle() {
        assert!(Rules::parse("").idle());
        assert!(Rules::parse("# nothing\n").idle());
        assert!(!Rules::parse("!dump").idle());
        assert!(!Rules::parse("!backdrop none").idle());
        assert!(!Rules::parse("Grid").idle());
    }

    #[test]
    fn backdrop() {
        assert_eq!(Rules::parse("!backdrop none\n").backdrop.as_deref(), Some("none"));
        assert_eq!(Rules::parse("!backdrop  #40FF0000 \n").backdrop.as_deref(), Some("#40FF0000"));
        assert_eq!(Rules::parse("!backdrop\n#A\n").backdrop, None);
    }
}
