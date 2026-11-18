//! Pure logic behind Start: the pins file, Windows' default pins, the All apps list with its letter
//! headers, the open easing, Recommended's relative times and the type-to-filter of step 1.

/// Windows 11's default Start pins (clean 24H2/25H2 install, no OEM layout), as AUMIDs. Pins whose
/// app is not installed are dropped when the list is resolved against `shell:AppsFolder`.
pub const DEFAULT_PINS: &[&str] = &[
    "MSEdge",
    "Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows",
    "Microsoft.Copilot_8wekyb3d8bbwe!App",
    "Microsoft.WindowsStore_8wekyb3d8bbwe!App",
    "Microsoft.Windows.Photos_8wekyb3d8bbwe!App",
    "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
    "Microsoft.Windows.Explorer",
    "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
    "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
    "Microsoft.ScreenSketch_8wekyb3d8bbwe!App",
    "Microsoft.Paint_8wekyb3d8bbwe!App",
    "Microsoft.WindowsAlarms_8wekyb3d8bbwe!App",
    "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
    "Microsoft.Todos_8wekyb3d8bbwe!App",
    "Clipchamp.Clipchamp_yxz26nhyzhsrt!App",
    "Microsoft.MicrosoftSolitaireCollection_8wekyb3d8bbwe!App",
    "Microsoft.GamingApp_8wekyb3d8bbwe!Microsoft.Xbox.App",
    "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic",
    "Microsoft.WindowsCamera_8wekyb3d8bbwe!App",
    "Microsoft.BingWeather_8wekyb3d8bbwe!App",
    "Microsoft.GetHelp_8wekyb3d8bbwe!App",
];

/// The pins file: `{"pins":["aumid",...]}`.
pub fn pins_to_json(pins: &[String]) -> String {
    let items: Vec<String> = pins.iter().map(|p| json_string(p)).collect();
    format!("{{\"pins\":[{}]}}\r\n", items.join(","))
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The strings of the `pins` array, or None when the file is not one we wrote.
pub fn pins_from_json(text: &str) -> Option<Vec<String>> {
    let start = text.find("\"pins\"")? + 6;
    let rest = text[start..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('[')?;
    let mut out = Vec::new();
    let mut chars = rest.chars();
    loop {
        match chars.find(|c| !c.is_whitespace() && *c != ',')? {
            ']' => return Some(out),
            '"' => {
                let mut s = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'u' => {
                                let hex: String = chars.by_ref().take(4).collect();
                                s.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                            }
                            c => s.push(c),
                        },
                        c => s.push(c),
                    }
                }
                out.push(s);
            }
            _ => return None,
        }
    }
}

/// The letter header an app name sorts under in All apps: A..Z, `#` for digits and the rest.
pub fn letter_of(name: &str) -> char {
    match name.chars().next().map(|c| c.to_uppercase().next().unwrap_or(c)) {
        Some(c) if c.is_alphabetic() => strip_accent(c),
        _ => '#',
    }
}

fn strip_accent(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' => 'N',
        'Ò'..='Ö' => 'O',
        'Ù'..='Ü' => 'U',
        _ => c,
    }
}

/// Sort key of an app name: by letter group (accents folded), `#` first as Windows lists them, then case-insensitive.
pub fn sort_key(name: &str) -> (bool, char, String) {
    let l = letter_of(name);
    (l != '#', l, name.to_lowercase())
}

/// A line of the All apps list.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Letter(char),
    /// Index into the sorted apps.
    App(usize),
}

/// Lines of All apps for names already sorted with `sort_key`.
pub fn all_apps_lines(sorted_names: &[&str]) -> Vec<Line> {
    let mut out = Vec::new();
    let mut last = None;
    for (i, n) in sorted_names.iter().enumerate() {
        let l = letter_of(n);
        if last != Some(l) {
            out.push(Line::Letter(l));
            last = Some(l);
        }
        out.push(Line::App(i));
    }
    out
}

/// Indices of the names matching a typed query: prefix of the name or of a word first, then
/// anywhere. Step 2 replaces this with the real search.
pub fn filter(names: &[&str], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<(u8, usize)> = names
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let l = n.to_lowercase();
            let rank = if l.starts_with(&q) {
                0
            } else if l.split([' ', '-', '.']).any(|w| w.starts_with(&q)) {
                1
            } else if l.contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, i))
        })
        .collect();
    ranked.sort();
    ranked.into_iter().map(|(_, i)| i).collect()
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` at time `t` in 0..1, Windows' decelerate curve for opening.
pub fn bezier(t: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let curve = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
    // Bisection on x(s) = t: monotonic for 0 <= x1, x2 <= 1.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if curve(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    curve(y1, y2, (lo + hi) / 2.0)
}

/// Fraction of the open slide done after `ms`.
pub fn open_progress(ms: f32, duration: f32) -> f32 {
    bezier(ms / duration, 0.1, 0.9, 0.2, 1.0)
}

/// Recommended's subtitle for an item touched `secs` ago (English, as Windows writes it).
pub fn ago(secs: i64, french: bool) -> String {
    let m = secs / 60;
    let h = m / 60;
    let d = h / 24;
    match (secs, french) {
        (s, false) if s < 60 => "Just now".into(),
        (s, true) if s < 60 => "À l'instant".into(),
        _ if m < 60 => if french { format!("Il y a {m} min") } else { format!("{m}m ago") },
        _ if h < 24 => if french { format!("Il y a {h} h") } else { format!("{h}h ago") },
        _ if d < 2 => if french { "Hier".into() } else { "Yesterday".into() },
        _ => if french { format!("Il y a {d} jours") } else { format!("{d} days ago") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip() {
        let pins = vec!["MSEdge".to_string(), "a\"b\\c".into(), "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()];
        assert_eq!(pins_from_json(&pins_to_json(&pins)).unwrap(), pins);
        assert_eq!(pins_from_json("{ \"pins\" : [ ] }").unwrap(), Vec::<String>::new());
        assert_eq!(pins_from_json("{\"pins\":[\"\\u0041\"]}").unwrap(), vec!["A".to_string()]);
        assert!(pins_from_json("{\"pins\":[1]}").is_none());
        assert!(pins_from_json("garbage").is_none());
    }

    #[test]
    fn letters_and_lines() {
        let mut names = vec!["notepad", "Calculator", "7-Zip", "Édition", "Clock"];
        names.sort_by_key(|n| sort_key(n));
        assert_eq!(names, vec!["7-Zip", "Calculator", "Clock", "Édition", "notepad"]);
        let lines = all_apps_lines(&names);
        assert_eq!(lines[0], Line::Letter('#'));
        assert_eq!(lines[2], Line::Letter('C'));
        assert_eq!(lines[5], Line::Letter('E'));
        assert_eq!(lines.len(), 5 + 4);
    }

    #[test]
    fn filter_ranks_prefix_first() {
        let names = ["Windows Terminal", "Terminal Preview", "Paint", "Notepad"];
        assert_eq!(filter(&names, "term"), vec![1, 0]);
        assert_eq!(filter(&names, "pa"), vec![2, 3]);
        assert!(filter(&names, " ").is_empty());
    }

    #[test]
    fn easing_ends_and_decelerates() {
        assert_eq!(open_progress(0.0, 250.0), 0.0);
        assert_eq!(open_progress(250.0, 250.0), 1.0);
        let half = open_progress(125.0, 250.0);
        assert!(half > 0.85 && half < 1.0, "{half}");
    }

    #[test]
    fn relative_times() {
        assert_eq!(ago(5, false), "Just now");
        assert_eq!(ago(17 * 60, false), "17m ago");
        assert_eq!(ago(3 * 3600, true), "Il y a 3 h");
        assert_eq!(ago(30 * 3600, false), "Yesterday");
        assert_eq!(ago(5 * 86400, false), "5 days ago");
    }
}
